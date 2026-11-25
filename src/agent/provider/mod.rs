//! AI providers: each turns its API's requests and replies into [`Action`]s and back
//! (docs/ai-computer-use-plan.md §5.4).

pub mod claude;

use core::future::Future;

use super::action::{Action, Scale};
use super::capture::Shot;

/// One action the model asked for, or why it could not be understood.
#[derive(Debug)]
pub struct Call {
    pub id: String,
    pub action: Result<Action, String>,
}

/// What the model said and the actions it asked for, in order; no actions ends its turn.
#[derive(Debug, Default)]
pub struct Reply {
    pub text: String,
    pub calls: Vec<Call>,
}

/// What an action produced for the model.
#[derive(Debug)]
pub enum Output {
    Text(String),
    Image(Shot),
}

#[derive(Debug)]
pub struct CallResult {
    pub id: String,
    /// The error is shown to the model, which can correct itself.
    pub outcome: Result<Output, String>,
}

/// A conversation with one model. Positions in calls are already on the remote screen.
pub trait Provider: Send {
    /// The person's message, with the current screen when there is one.
    fn user(&mut self, text: &str, screen: Option<&Shot>);

    /// The results of the last reply's calls, in order; `screen` goes with the last one so the
    /// model sees where the actions left the screen.
    fn results(&mut self, results: Vec<CallResult>, screen: Option<&Shot>);

    /// Asks the model for its next turn; `scale` maps its positions onto the remote screen.
    fn next(&mut self, scale: Scale) -> impl Future<Output = anyhow::Result<Reply>> + Send;
}
