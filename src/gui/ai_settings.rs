//! AI settings: whether RDP sessions show the AI assistant, which model it uses, and its API key
//! (kept in the OS credential store, never in the settings file).

use eframe::egui::{self, Id, Key, RichText};

use super::theme::{Bold as _, palette};
use super::{platform, widgets};
use crate::agent::API_KEY_ACCOUNT;
use crate::agent::provider::claude::{DEFAULT_MODEL, MODELS};
use crate::profile::{AiSettings, ProfileStore};

/// What the settings window ended with.
pub(super) enum Outcome {
    Open,
    /// Saved; the text is for a toast.
    Saved(String),
    Cancelled,
}

pub(super) struct AiForm {
    enabled: bool,
    model: &'static str,
    /// A new key; empty keeps the stored one.
    key: String,
    has_key: bool,
    error: Option<String>,
}

impl AiForm {
    pub(super) fn open(store: &ProfileStore) -> Self {
        let settings = store.ai();
        let model = MODELS
            .iter()
            .map(|(id, _)| *id)
            .find(|id| *id == settings.model)
            .unwrap_or(DEFAULT_MODEL);
        let has_key = crate::secret_store::get(API_KEY_ACCOUNT).is_ok_and(|key| key.is_some_and(|k| !k.trim().is_empty()));
        Self {
            enabled: settings.enabled,
            model,
            key: String::new(),
            has_key,
            error: None,
        }
    }

    pub(super) fn ui(&mut self, ctx: &egui::Context, store: &mut ProfileStore) -> Outcome {
        let mut outcome = Outcome::Open;
        let modal = egui::Modal::new(Id::unique("ai_settings")).show(ctx, |ui| {
            let pal = palette(ui);
            ui.set_width(420.0);
            ui.label(RichText::new("AI 공동 작업자").size(16.0).bold().color(pal.text));
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "원격 데스크톱(RDP) 창에 AI 캐릭터를 띄워, 대화로 원격 PC를 함께 조작합니다. \
                     켜면 원격 화면이 Anthropic API(api.anthropic.com)로 전송되고, \
                     AI의 모든 동작은 실행 전에 확인을 받습니다.",
                )
                .color(pal.text_weak),
            );
            ui.add_space(12.0);
            ui.checkbox(&mut self.enabled, "RDP 세션에서 AI 사용");
            ui.add_space(10.0);

            widgets::field(ui, "모델", None, |ui| {
                let options: Vec<(&'static str, &str)> = MODELS.iter().map(|(id, name)| (*id, *name)).collect();
                widgets::segmented(ui, &mut self.model, &options);
                ui.label(RichText::new("Opus가 가장 정확하고, Haiku가 가장 빠르고 저렴합니다.").small().color(pal.text_weak));
            });
            ui.add_space(10.0);

            let hint = if self.has_key { "저장된 키 사용 (바꾸려면 새 키 입력)" } else { "sk-ant-…" };
            let width = ui.available_width();
            let error = self.error.clone();
            widgets::field(ui, "Anthropic API 키", error.as_deref(), |ui| {
                ui.add(widgets::text_input(ui, &mut self.key, hint, width, error.is_some()).password(true));
                ui.horizontal(|ui| {
                    let status = if self.has_key {
                        format!("{}에 저장됨", platform::SECRET_STORE)
                    } else {
                        "저장된 키 없음".to_owned()
                    };
                    ui.label(RichText::new(status).small().color(pal.text_weak));
                    if self.has_key && ui.small_button("키 삭제").clicked() {
                        match crate::secret_store::delete(&[API_KEY_ACCOUNT.to_owned()]) {
                            Ok(()) => self.has_key = false,
                            Err(error) => self.error = Some(format!("키를 삭제하지 못했습니다: {error:#}")),
                        }
                    }
                });
            });
            ui.add_space(4.0);
            ui.label(RichText::new("바꾼 설정은 새로 여는 세션부터 적용됩니다.").small().color(pal.text_weak));
            ui.add_space(14.0);

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::primary_button(ui, "저장").clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                    if let Some(toast) = self.save(store) {
                        outcome = Outcome::Saved(toast);
                    }
                }
                if ui.button("취소").clicked() {
                    outcome = Outcome::Cancelled;
                }
            });
        });
        if modal.should_close() && matches!(outcome, Outcome::Open) {
            outcome = Outcome::Cancelled;
        }
        outcome
    }

    /// Stores the key and settings; `None` keeps the window open with an error.
    fn save(&mut self, store: &mut ProfileStore) -> Option<String> {
        let key = self.key.trim();
        if !key.is_empty() {
            if let Err(error) = crate::secret_store::set(API_KEY_ACCOUNT, key) {
                self.error = Some(format!("키를 저장하지 못했습니다: {error:#}"));
                return None;
            }
            self.has_key = true;
            self.key.clear();
        }
        if self.enabled && !self.has_key {
            self.error = Some("AI를 켜려면 API 키를 입력하세요.".to_owned());
            return None;
        }
        let settings = AiSettings {
            enabled: self.enabled,
            model: self.model.to_owned(),
        };
        if let Err(error) = store.set_ai(settings) {
            self.error = Some(format!("설정을 저장하지 못했습니다: {error:#}"));
            return None;
        }
        Some(if self.enabled { "AI 공동 작업자를 켰습니다".to_owned() } else { "AI 공동 작업자를 껐습니다".to_owned() })
    }
}
