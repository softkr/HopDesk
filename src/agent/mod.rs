//! The AI co-worker: a model looks at the remote screen and operates it alongside the person
//! (docs/ai-computer-use-plan.md).
//!
//! [`run`] is the conversation loop. The session supplies the screen and input through
//! [`Remote`]; the person's commands come in as [`Command`]s and what the agent does goes out as
//! [`Event`]s for the overlay.

pub mod action;
pub mod capture;
pub mod provider;

use core::future::Future;
use core::time::Duration;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;

use self::action::{Action, Coordinates, Point, Scale};
use self::capture::{Pixels, Shot};
use self::provider::{Call, CallResult, Output, Provider};

/// Model turns with actions per task, so a model that loops cannot run up costs unattended.
pub const MAX_STEPS: usize = 50;
/// How often a wait checks whether the person took over or stopped.
const WAIT_POLL: Duration = Duration::from_millis(100);
/// The screen settles after input before the model looks at it.
const SETTLE: Duration = Duration::from_millis(400);

const NOT_EXECUTED: &str = "Not executed: an earlier computer action in this turn failed.";
const TAKEN_OVER: &str = "Not executed: the person took over the mouse or keyboard. Look at the screen again before \
                          continuing.";
const STOPPED: &str = "Not executed: the person stopped the task.";
const DECLINED: &str = "Not executed: the person declined these actions.";
/// Sent when the person resumes after taking over.
const RESUME: &str = "I'm done; please continue the task from the current screen.";

/// What the person tells the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// A new task, or more instructions for the current one.
    Say(String),
    /// Carry out the proposed actions, or resume after the person took over.
    Approve,
    /// Skip the proposed actions.
    Decline,
    /// End the task now.
    Stop,
    /// The person is busy on the screen: proposed actions wait for an answer instead of
    /// running by themselves.
    Hold,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Waiting for a task.
    Idle,
    /// Waiting for the model.
    Thinking,
    /// Operating the screen.
    Acting,
    /// Waiting for the person to approve the proposed actions.
    Approval,
    /// The person took over; waiting for them to resume.
    TakenOver,
}

/// What the agent reports to the overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    State(State),
    /// The model's words.
    Say(String),
    /// A message from HopDesk itself, such as an error.
    Notice(String),
    /// Actions waiting for approval: a description, and where on the screen each one acts.
    Proposal(Vec<(String, Option<Point>)>),
    /// Where the AI's pointer is on the remote screen.
    Pointer(Point),
    /// The proposed actions run by themselves after this long unless declined; `None` once
    /// they wait for an answer after all.
    AutoApprove(Option<Duration>),
}

/// The remote screen and input of a session.
pub trait Remote: Send {
    fn snapshot(&mut self) -> impl Future<Output = anyhow::Result<Pixels>> + Send;
    /// Sends the input of an action (not screenshots, zooms or waits).
    fn act(&mut self, action: &Action) -> impl Future<Output = anyhow::Result<()>> + Send;
    /// The AI's pointer on the remote screen.
    fn cursor(&self) -> Point;
}

pub struct Options {
    /// Every batch of actions waits for the person's approval.
    pub approve: bool,
    /// Proposed actions run by themselves when the person does not decline within this time.
    pub auto_approve: Option<Duration>,
    pub coordinates: Coordinates,
}

/// How a task ended.
enum Ended {
    Done,
    Stopped,
    Failed(String),
}

/// Runs the conversation until the session closes `commands`.
///
/// `taken_over` is set by the session when the person uses the mouse or keyboard while the agent
/// acts; the agent clears it.
pub async fn run<P: Provider, R: Remote>(
    mut provider: P,
    mut remote: R,
    commands: mpsc::UnboundedReceiver<Command>,
    taken_over: Arc<AtomicBool>,
    emit: impl Fn(Event) + Send + Sync,
    options: Options,
) {
    let mut agent = Agent {
        remote: &mut remote,
        taken_over: &taken_over,
        emit: &emit,
        coordinates: options.coordinates,
        approve: options.approve,
        auto_approve: options.auto_approve,
        scale: None,
    };
    let mut commands = Commands {
        receiver: commands,
        held: VecDeque::new(),
    };
    emit(Event::State(State::Idle));
    let mut next_message: Option<String> = None;
    loop {
        let text = match next_message.take() {
            Some(text) => text,
            None => match commands.recv().await {
                Some(Command::Say(text)) => text,
                Some(_) => continue,
                None => return,
            },
        };
        let ended = agent.task(&mut provider, &mut commands, text, &mut next_message).await;
        match ended {
            Ended::Done => {}
            Ended::Stopped => emit(Event::Notice("작업을 멈췄습니다.".into())),
            Ended::Failed(error) => emit(Event::Notice(format!("오류: {error}"))),
        }
        emit(Event::State(State::Idle));
    }
}

struct Agent<'a, R, E> {
    remote: &'a mut R,
    taken_over: &'a AtomicBool,
    emit: &'a E,
    coordinates: Coordinates,
    approve: bool,
    auto_approve: Option<Duration>,
    /// How the last screenshot the model saw maps onto the screen.
    scale: Option<Scale>,
}

impl<R: Remote, E: Fn(Event) + Send + Sync> Agent<'_, R, E> {
    /// One task, from the person's message until the model ends its turn or the task stops.
    /// A message the person sends while asked for approval ends the task and is left in `next`.
    async fn task<P: Provider>(
        &mut self,
        provider: &mut P,
        commands: &mut Commands,
        text: String,
        next: &mut Option<String>,
    ) -> Ended {
        self.taken_over.store(false, Ordering::Relaxed);
        let screen = match self.screenshot().await {
            Ok(shot) => shot,
            Err(error) => return Ended::Failed(format!("{error:#}")),
        };
        provider.user(&text, Some(&screen));
        for _ in 0..MAX_STEPS {
            (self.emit)(Event::State(State::Thinking));
            let Some(scale) = self.scale else {
                return Ended::Failed("no screen".into());
            };
            let reply = tokio::select! {
                biased;
                reply = provider.next(scale) => reply,
                stop = wait_for_stop(commands) => {
                    if let Some(text) = stop {
                        *next = Some(text);
                    }
                    return Ended::Stopped;
                }
            };
            let reply = match reply {
                Ok(reply) => reply,
                Err(error) => return Ended::Failed(format!("{error:#}")),
            };
            if !reply.text.is_empty() {
                (self.emit)(Event::Say(reply.text));
            }
            if reply.calls.is_empty() {
                return Ended::Done;
            }
            // A provider's safety check always asks the person, also without approval mode.
            let confirm = reply.calls.iter().any(|call| call.confirm.is_some());
            if confirm || (self.approve && reply.calls.iter().any(needs_approval)) {
                (self.emit)(Event::Proposal(reply.calls.iter().flat_map(describe).collect()));
                (self.emit)(Event::State(State::Approval));
                // A provider's safety check is never answered by a timer.
                let auto = if confirm { None } else { self.auto_approve };
                match self.approval(commands, auto).await {
                    Approval::Approved => {}
                    approval => {
                        let reason = if matches!(approval, Approval::Stopped(_)) { STOPPED } else { DECLINED };
                        provider.results(refuse_all(&reply.calls, reason), None);
                        return match approval {
                            Approval::Declined => Ended::Done,
                            Approval::Stopped(text) => {
                                *next = text;
                                Ended::Stopped
                            }
                            Approval::Approved => unreachable!(),
                        };
                    }
                }
            }
            (self.emit)(Event::State(State::Acting));
            let (results, stopped) = self.carry_out(&reply.calls, commands).await;
            let screen = self.screenshot().await.ok();
            provider.results(results, screen.as_ref());
            if stopped {
                return Ended::Stopped;
            }
            if self.taken_over.swap(false, Ordering::Relaxed) {
                (self.emit)(Event::State(State::TakenOver));
                (self.emit)(Event::Notice(
                    "직접 조작하셔서 일시 정지했습니다. 이어서 하려면 '계속'을 누르세요.".into(),
                ));
                match self.approval(commands, None).await {
                    Approval::Approved => {
                        provider.user(RESUME, self.screenshot().await.ok().as_ref())
                    }
                    Approval::Declined | Approval::Stopped(None) => return Ended::Stopped,
                    Approval::Stopped(Some(text)) => {
                        provider.user(&text, self.screenshot().await.ok().as_ref());
                    }
                }
            }
        }
        Ended::Failed(format!("한 작업에 {MAX_STEPS}단계를 넘어 멈췄습니다."))
    }

    /// Waits for the person's answer; with `auto`, no answer in that time approves, unless the
    /// person is busy on the screen ([`Command::Hold`]).
    async fn approval(&self, commands: &mut Commands, auto: Option<Duration>) -> Approval {
        let mut deadline = auto.map(|after| tokio::time::Instant::now() + after);
        if auto.is_some() {
            (self.emit)(Event::AutoApprove(auto));
        }
        loop {
            let command = match deadline {
                Some(at) => tokio::select! {
                    biased;
                    command = commands.recv() => command,
                    () = tokio::time::sleep_until(at) => return Approval::Approved,
                },
                None => commands.recv().await,
            };
            match command {
                Some(Command::Approve) => return Approval::Approved,
                Some(Command::Decline) => return Approval::Declined,
                Some(Command::Say(text)) => return Approval::Stopped(Some(text)),
                Some(Command::Stop) | None => return Approval::Stopped(None),
                Some(Command::Hold) => {
                    if deadline.take().is_some() {
                        (self.emit)(Event::AutoApprove(None));
                    }
                }
            }
        }
    }

    /// Carries out the calls in order, up to the first failure, takeover or stop; returns one
    /// result per call and whether the person stopped the task.
    async fn carry_out(
        &mut self,
        calls: &[Call],
        commands: &mut Commands,
    ) -> (Vec<CallResult>, bool) {
        let mut results = Vec::with_capacity(calls.len());
        let mut failed = false;
        let mut stopped = false;
        for call in calls {
            stopped |= commands.stop_requested();
            let outcome = if stopped {
                Err(STOPPED.to_owned())
            } else if failed {
                Err(NOT_EXECUTED.to_owned())
            } else if self.taken_over.load(Ordering::Relaxed) {
                failed = true;
                Err(TAKEN_OVER.to_owned())
            } else {
                match &call.actions {
                    Ok(actions) => self.carry_out_call(actions, commands).await,
                    Err(error) => Err(error.clone()),
                }
            };
            failed |= outcome.is_err();
            results.push(CallResult {
                id: call.id.clone(),
                outcome,
            });
        }
        (results, stopped)
    }

    /// The actions of one call in order; the call's result is the last action's output, or the
    /// first failure.
    async fn carry_out_call(&mut self, actions: &[Action], commands: &mut Commands) -> Result<Output, String> {
        let mut output = Output::Text("OK".into());
        for (index, action) in actions.iter().enumerate() {
            if index > 0 && (self.taken_over.load(Ordering::Relaxed) || commands.stop_requested()) {
                return Err(TAKEN_OVER.to_owned());
            }
            output = self.carry_out_one(action).await.map_err(|error| format!("{error:#}"))?;
        }
        Ok(output)
    }

    async fn carry_out_one(&mut self, action: &Action) -> anyhow::Result<Output> {
        match action {
            Action::Screenshot => Ok(Output::Image(self.screenshot().await?)),
            Action::Zoom { region } => {
                let pixels = self.remote.snapshot().await?;
                Ok(Output::Image(capture::capture(&pixels, *region)?))
            }
            Action::CursorPosition => {
                let scale = self.scale.ok_or_else(|| anyhow::anyhow!("no screen"))?;
                let (x, y) = scale.to_model(self.remote.cursor());
                Ok(Output::Text(format!("[{x}, {y}]")))
            }
            Action::Wait { duration } => {
                let end = tokio::time::Instant::now() + *duration;
                while tokio::time::Instant::now() < end && !self.taken_over.load(Ordering::Relaxed) {
                    tokio::time::sleep(WAIT_POLL.min(end - tokio::time::Instant::now())).await;
                }
                Ok(Output::Text("OK".into()))
            }
            _ => {
                self.remote.act(action).await?;
                (self.emit)(Event::Pointer(self.remote.cursor()));
                Ok(Output::Text("OK".into()))
            }
        }
    }

    /// The whole screen for the model, after input settled; remembers its scale.
    async fn screenshot(&mut self) -> anyhow::Result<Shot> {
        tokio::time::sleep(SETTLE).await;
        let pixels = self.remote.snapshot().await?;
        let shot = capture::screenshot(&pixels)?;
        self.scale = Some(Scale::new((pixels.width, pixels.height), (shot.width, shot.height), self.coordinates));
        Ok(shot)
    }
}

/// The person's commands; ones that arrive while the agent acts wait for their turn.
struct Commands {
    receiver: mpsc::UnboundedReceiver<Command>,
    held: VecDeque<Command>,
}

impl Commands {
    async fn recv(&mut self) -> Option<Command> {
        match self.held.pop_front() {
            Some(command) => Some(command),
            None => self.receiver.recv().await,
        }
    }

    /// Whether the person asked to stop; other commands are kept for later.
    fn stop_requested(&mut self) -> bool {
        while let Ok(command) = self.receiver.try_recv() {
            self.held.push_back(command);
        }
        let before = self.held.len();
        self.held.retain(|command| *command != Command::Stop);
        self.held.len() != before
    }
}

enum Approval {
    Approved,
    Declined,
    /// With the person's new message, if they wrote one instead.
    Stopped(Option<String>),
}

/// Resolves when the person stops the task or writes something new (returned).
async fn wait_for_stop(commands: &mut Commands) -> Option<String> {
    loop {
        match commands.recv().await {
            Some(Command::Stop) | None => return None,
            Some(Command::Say(text)) => return Some(text),
            Some(Command::Approve | Command::Decline | Command::Hold) => {}
        }
    }
}

/// Looking at the screen and waiting change nothing, so they need no approval.
fn needs_approval(call: &Call) -> bool {
    let observes = |action: &Action| {
        matches!(action, Action::Screenshot | Action::Zoom { .. } | Action::CursorPosition | Action::Wait { .. })
    };
    call.confirm.is_some() || call.actions.as_ref().is_ok_and(|actions| !actions.iter().all(observes))
}

fn refuse_all(calls: &[Call], reason: &str) -> Vec<CallResult> {
    calls
        .iter()
        .map(|call| CallResult {
            id: call.id.clone(),
            outcome: Err(reason.to_owned()),
        })
        .collect()
}

/// Short Korean descriptions of a call's actions for the approval list, and where each acts;
/// a provider's safety check comes first.
fn describe(call: &Call) -> Vec<(String, Option<Point>)> {
    let mut lines: Vec<(String, Option<Point>)> = Vec::new();
    if let Some(check) = &call.confirm {
        lines.push((format!("⚠ 확인 필요: {check}"), None));
    }
    match &call.actions {
        Ok(actions) => lines.extend(actions.iter().map(describe_action)),
        Err(_) => lines.push(("(알 수 없는 동작)".into(), None)),
    }
    lines
}

fn describe_action(action: &Action) -> (String, Option<Point>) {
    match action {
        Action::Screenshot => ("화면 보기".into(), None),
        Action::Zoom { .. } => ("화면 확대해 보기".into(), None),
        Action::CursorPosition => ("포인터 위치 확인".into(), None),
        Action::Click { at, button, count, .. } => {
            let what = match (button, count) {
                (action::Button::Left, 2) => "더블클릭",
                (action::Button::Left, 3) => "세 번 클릭",
                (action::Button::Left, _) => "클릭",
                (action::Button::Right, _) => "오른쪽 클릭",
                (action::Button::Middle, _) => "가운데 클릭",
            };
            (what.into(), *at)
        }
        Action::Drag { to, .. } => ("끌어서 옮기기".into(), Some(*to)),
        Action::MouseMove { to } => ("포인터 이동".into(), Some(*to)),
        Action::MouseDown => ("마우스 누르기".into(), None),
        Action::MouseUp => ("마우스 떼기".into(), None),
        Action::Scroll { at, .. } => ("스크롤".into(), *at),
        Action::Type { text } => {
            let short: String = text.chars().take(30).collect();
            let more = if text.chars().count() > 30 { "…" } else { "" };
            (format!("입력: \"{short}{more}\""), None)
        }
        Action::Key { .. } => ("키 누르기".into(), None),
        Action::Wait { .. } => ("기다리기".into(), None),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;
    use crate::agent::action::Button;
    use crate::agent::provider::Reply;

    /// A model that plays back replies and records what it was told.
    struct Script {
        replies: VecDeque<Reply>,
        log: Arc<Mutex<Vec<String>>>,
    }

    impl Provider for Script {
        fn user(&mut self, text: &str, screen: Option<&Shot>) {
            self.log.lock().unwrap().push(format!("user {text} screen={}", screen.is_some()));
        }

        fn results(&mut self, results: Vec<CallResult>, screen: Option<&Shot>) {
            for result in results {
                let outcome = match result.outcome {
                    Ok(Output::Text(text)) => text,
                    Ok(Output::Image(_)) => "image".into(),
                    Err(error) => format!("error {error}"),
                };
                self.log.lock().unwrap().push(format!("result {} {outcome}", result.id));
            }
            self.log.lock().unwrap().push(format!("screen={}", screen.is_some()));
        }

        async fn next(&mut self, _scale: Scale) -> anyhow::Result<Reply> {
            Ok(self.replies.pop_front().unwrap_or_default())
        }
    }

    /// A 4x3 screen that records the actions it receives.
    struct Screen {
        acted: Arc<Mutex<Vec<Action>>>,
        /// Set the takeover flag when an action arrives, like a person grabbing the mouse.
        take_over: Option<Arc<AtomicBool>>,
    }

    impl Remote for Screen {
        async fn snapshot(&mut self) -> anyhow::Result<Pixels> {
            Ok(Pixels {
                width: 4,
                height: 3,
                rgba: vec![255; 48],
            })
        }

        async fn act(&mut self, action: &Action) -> anyhow::Result<()> {
            self.acted.lock().unwrap().push(action.clone());
            if let Some(flag) = &self.take_over {
                flag.store(true, Ordering::Relaxed);
            }
            Ok(())
        }

        fn cursor(&self) -> Point {
            Point { x: 1, y: 1 }
        }
    }

    fn click(id: &str) -> Call {
        Call::one(
            id.into(),
            Ok(Action::Click {
                at: Some(Point { x: 1, y: 2 }),
                button: Button::Left,
                count: 1,
                modifiers: vec![],
            }),
        )
    }

    struct Run {
        log: Vec<String>,
        acted: Vec<Action>,
        events: Vec<Event>,
    }

    /// Runs the agent on `replies` with `commands` already queued, until they run out.
    fn run_script(
        replies: Vec<Reply>,
        commands: Vec<Command>,
        approve: bool,
        take_over: bool,
        auto_approve: Option<Duration>,
    ) -> Run {
        let log = Arc::new(Mutex::new(Vec::new()));
        let acted = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let taken_over = Arc::new(AtomicBool::new(false));
        let provider = Script {
            replies: replies.into(),
            log: Arc::clone(&log),
        };
        let screen = Screen {
            acted: Arc::clone(&acted),
            take_over: take_over.then(|| Arc::clone(&taken_over)),
        };
        let (sender, receiver) = mpsc::unbounded_channel();
        for command in commands {
            sender.send(command).unwrap();
        }
        let sink = Arc::clone(&events);
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().start_paused(true).build().unwrap();
        rt.block_on(async {
            let agent = tokio::spawn(run(
                provider,
                screen,
                receiver,
                taken_over,
                move |event| sink.lock().unwrap().push(event),
                Options {
                    approve,
                    auto_approve,
                    coordinates: Coordinates::Pixels,
                },
            ));
            // Time only moves on once the agent waits for the person: then the session closes.
            tokio::time::sleep(Duration::from_secs(3600)).await;
            drop(sender);
            agent.await.unwrap();
        });
        fn take<T>(recorded: &Mutex<Vec<T>>) -> Vec<T> {
            core::mem::take(&mut *recorded.lock().unwrap())
        }
        Run {
            log: take(&log),
            acted: take(&acted),
            events: take(&events),
        }
    }

    #[test]
    fn a_task_runs_until_the_model_ends_its_turn() {
        let replies = vec![
            Reply {
                text: "클릭할게요".into(),
                calls: vec![click("a"), click("b")],
            },
            Reply {
                text: "끝났어요".into(),
                calls: vec![],
            },
        ];
        let run = run_script(
            replies,
            vec![Command::Say("해 줘".into())],
            false,
            false,
            None,
        );
        assert_eq!(run.acted.len(), 2);
        assert_eq!(
            run.log,
            ["user 해 줘 screen=true", "result a OK", "result b OK", "screen=true"]
        );
        assert!(run.events.contains(&Event::Say("끝났어요".into())));
        assert_eq!(run.events.last(), Some(&Event::State(State::Idle)));
    }

    #[test]
    fn a_failed_call_skips_the_rest_of_the_batch() {
        let broken = Call::one("a".into(), Err("bad coordinate".into()));
        let replies = vec![Reply {
            text: String::new(),
            calls: vec![broken, click("b")],
        }];
        let run = run_script(replies, vec![Command::Say("go".into())], false, false, None);
        assert!(run.acted.is_empty());
        assert_eq!(run.log[1], "result a error bad coordinate");
        assert_eq!(run.log[2], format!("result b error {NOT_EXECUTED}"));
    }

    #[test]
    fn actions_wait_for_approval_and_can_be_declined() {
        let replies = vec![Reply {
            text: String::new(),
            calls: vec![click("a")],
        }];
        let run = run_script(
            replies,
            vec![Command::Say("go".into()), Command::Decline],
            true,
            false,
            None,
        );
        assert!(run.acted.is_empty());
        assert!(run.events.iter().any(|event| matches!(event, Event::Proposal(list) if list[0].0 == "클릭")));
        assert_eq!(run.log[1], format!("result a error {DECLINED}"));
    }

    #[test]
    fn unanswered_actions_run_by_themselves_when_asked_to() {
        let reply = || {
            vec![Reply {
                text: String::new(),
                calls: vec![click("a")],
            }]
        };
        let auto = Some(Duration::from_secs(3));
        let say = || Command::Say("go".into());
        let run = run_script(reply(), vec![say()], true, false, auto);
        assert_eq!(run.acted.len(), 1, "nobody declined in time");
        assert!(run.events.contains(&Event::AutoApprove(auto)));
        let run = run_script(reply(), vec![say(), Command::Decline], true, false, auto);
        assert!(run.acted.is_empty(), "declined in time");
        let run = run_script(reply(), vec![say(), Command::Hold], true, false, auto);
        assert!(
            run.acted.is_empty(),
            "the person is busy: it waits for an answer"
        );
        assert!(run.events.contains(&Event::AutoApprove(None)));
    }

    #[test]
    fn a_safety_check_is_never_approved_by_the_timer() {
        let mut call = click("a");
        call.confirm = Some("Suspicious".into());
        let replies = vec![Reply {
            text: String::new(),
            calls: vec![call],
        }];
        let run = run_script(
            replies,
            vec![Command::Say("go".into())],
            true,
            false,
            Some(Duration::from_secs(3)),
        );
        assert!(run.acted.is_empty());
    }

    #[test]
    fn approved_actions_are_carried_out() {
        let replies = vec![Reply {
            text: String::new(),
            calls: vec![click("a")],
        }];
        let run = run_script(
            replies,
            vec![Command::Say("go".into()), Command::Approve],
            true,
            false,
            None,
        );
        assert_eq!(run.acted.len(), 1);
    }

    #[test]
    fn the_person_taking_over_stops_the_batch_until_they_resume() {
        let replies = vec![
            Reply {
                text: String::new(),
                calls: vec![click("a"), click("b")],
            },
            Reply::default(),
        ];
        let run = run_script(
            replies,
            vec![Command::Say("go".into()), Command::Approve],
            false,
            true,
            None,
        );
        assert_eq!(run.acted.len(), 1, "the second click waits for the person");
        assert_eq!(run.log[2], format!("result b error {TAKEN_OVER}"));
        assert!(run.events.contains(&Event::State(State::TakenOver)));
        assert_eq!(run.log[4], format!("user {RESUME} screen=true"));
    }
}
