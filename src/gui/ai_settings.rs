//! AI settings: whether RDP sessions show the AI assistant, which provider and model it uses,
//! and the API keys (kept in the OS credential store, never in the settings file).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use eframe::egui::{self, Id, Key, RichText};

use super::theme::{Bold as _, palette};
use super::{platform, widgets};
use crate::agent::action::Coordinates;
use crate::agent::provider::{self, Kind};
use crate::profile::{AiSettings, ProfileStore};

/// What the settings window ended with.
pub(super) enum Outcome {
    Open,
    /// Saved; the text is for a toast.
    Saved(String),
    Cancelled,
}

/// The window's height left for the buttons and margins outside the scrolled part.
const OUTSIDE_SCROLL: f32 = 140.0;

/// A model list being fetched for a provider, then its models or why it failed.
type Listing = Arc<Mutex<Option<Result<Vec<String>, String>>>>;

/// The provider tabs; a compatible server is one tab with a choice of API format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Claude,
    Gpt,
    Gemini,
    Server,
}

impl Tab {
    fn of(kind: Kind) -> Self {
        match kind {
            Kind::Anthropic => Self::Claude,
            Kind::OpenAi => Self::Gpt,
            Kind::Gemini => Self::Gemini,
            Kind::OpenAiCompatible | Kind::AnthropicCompatible => Self::Server,
        }
    }
}

pub(super) struct AiForm {
    enabled: bool,
    tab: Tab,
    /// The compatible server's API format.
    server_kind: Kind,
    /// The model typed for each provider, kept while switching tabs.
    models: HashMap<Kind, String>,
    base_url: String,
    coordinates: Coordinates,
    /// A new key for the selected provider; empty keeps the stored one.
    key: String,
    /// Whether each provider has a key stored, looked up once.
    stored: HashMap<Kind, bool>,
    /// The model list fetched last, with the settings it was fetched for.
    listing: Option<(Kind, String, Listing)>,
    error: Option<String>,
}

impl AiForm {
    pub(super) fn open(store: &ProfileStore) -> Self {
        let settings = store.ai();
        let mut models = HashMap::new();
        models.insert(settings.kind, settings.model.clone());
        Self {
            enabled: settings.enabled,
            tab: Tab::of(settings.kind),
            server_kind: if settings.kind == Kind::AnthropicCompatible { Kind::AnthropicCompatible } else { Kind::OpenAiCompatible },
            models,
            base_url: settings.base_url.clone(),
            coordinates: settings.coordinates,
            key: String::new(),
            stored: HashMap::new(),
            listing: None,
            error: None,
        }
    }

    /// Fetches the model list in the background with the typed key, else the stored one.
    fn load_models(&mut self, ctx: &egui::Context) {
        let settings = self.settings();
        let typed = self.key.trim().to_owned();
        let listing: Listing = Arc::new(Mutex::new(None));
        self.listing = Some((settings.kind, settings.base_url.clone(), Arc::clone(&listing)));
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let key = if typed.is_empty() {
                crate::secret_store::get(settings.kind.key_account()).ok().flatten().unwrap_or_default()
            } else {
                typed
            };
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(anyhow::Error::from)
                .and_then(|runtime| runtime.block_on(provider::list_models(&settings, key.trim())))
                .map_err(|error| format!("{error:#}"));
            *listing.lock().unwrap_or_else(PoisonError::into_inner) = Some(result);
            ctx.request_repaint();
        });
    }

    fn kind(&self) -> Kind {
        match self.tab {
            Tab::Claude => Kind::Anthropic,
            Tab::Gpt => Kind::OpenAi,
            Tab::Gemini => Kind::Gemini,
            Tab::Server => self.server_kind,
        }
    }

    fn has_key(&mut self, kind: Kind) -> bool {
        *self.stored.entry(kind).or_insert_with(|| {
            crate::secret_store::get(kind.key_account()).is_ok_and(|key| key.is_some_and(|k| !k.trim().is_empty()))
        })
    }

    fn settings(&self) -> AiSettings {
        let kind = self.kind();
        AiSettings {
            enabled: self.enabled,
            kind,
            model: self.models.get(&kind).cloned().unwrap_or_else(|| kind.default_model().to_owned()).trim().to_owned(),
            base_url: self.base_url.trim().to_owned(),
            coordinates: self.coordinates,
        }
    }

    pub(super) fn ui(&mut self, ctx: &egui::Context, store: &mut ProfileStore) -> Outcome {
        let mut outcome = Outcome::Open;
        let modal = egui::Modal::new(Id::unique("ai_settings")).show(ctx, |ui| {
            let pal = palette(ui);
            ui.set_width(460.0);
            ui.label(RichText::new("AI 공동 작업자").size(16.0).bold().color(pal.text));
            // A small window scrolls the settings; the buttons below stay in view.
            let max_height = (ctx.content_rect().height() - OUTSIDE_SCROLL).max(160.0);
            egui::ScrollArea::vertical().max_height(max_height).auto_shrink([false, true]).show(ui, |ui| {
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!(
                    "원격 데스크톱(RDP) 창에 AI 캐릭터를 띄워, 대화로 원격 PC를 함께 조작합니다. \
                     켜면 원격 화면이 {}로 전송되고, AI의 모든 동작은 실행 전에 확인을 받습니다.",
                    provider::destination(&self.settings())
                ))
                .color(pal.text_weak),
            );
            ui.add_space(12.0);
            ui.checkbox(&mut self.enabled, "RDP 세션에서 AI 사용");
            ui.add_space(10.0);

            widgets::field(ui, "AI 제공자", None, |ui| {
                let tabs = [(Tab::Claude, "Claude"), (Tab::Gpt, "GPT"), (Tab::Gemini, "Gemini"), (Tab::Server, "내 서버 (오픈 모델)")];
                if widgets::segmented(ui, &mut self.tab, &tabs) {
                    self.key.clear();
                    self.error = None;
                }
            });
            ui.add_space(10.0);

            // The key follows the server address, or comes first without one; then the model,
            // which "모델 불러오기" lists with that key.
            if self.tab == Tab::Server {
                self.server_fields(ui);
                ui.add_space(10.0);
            }
            self.key_field(ui);
            ui.add_space(10.0);
            self.model_field(ui);
            if self.tab == Tab::Server {
                ui.add_space(10.0);
                self.coordinates_field(ui);
            }
            ui.add_space(4.0);
            ui.label(RichText::new("바꾼 설정은 새로 여는 세션부터 적용됩니다.").small().color(pal.text_weak));
            });
            ui.add_space(14.0);

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if (widgets::primary_button(ui, "저장").clicked() || ui.input(|i| i.key_pressed(Key::Enter)))
                    && let Some(toast) = self.save(store)
                {
                    outcome = Outcome::Saved(toast);
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

    fn server_fields(&mut self, ui: &mut egui::Ui) {
        let pal = palette(ui);
        widgets::field(ui, "API 형식", None, |ui| {
            let formats = [(Kind::OpenAiCompatible, "OpenAI 형식"), (Kind::AnthropicCompatible, "Claude 형식")];
            widgets::segmented(ui, &mut self.server_kind, &formats);
            ui.label(RichText::new("Ollama, vLLM, LM Studio 등은 대개 OpenAI 형식입니다.").small().color(pal.text_weak));
        });
        ui.add_space(10.0);
        let width = ui.available_width();
        widgets::field(ui, "서버 주소", None, |ui| {
            ui.add(widgets::text_input(ui, &mut self.base_url, "http://localhost:11434", width, false));
            ui.label(RichText::new("https 주소, 또는 이 기기·사내망의 http 주소").small().color(pal.text_weak));
        });
    }

    fn coordinates_field(&mut self, ui: &mut egui::Ui) {
        let pal = palette(ui);
        widgets::field(ui, "좌표 방식", None, |ui| {
            let options = [(Coordinates::Pixels, "스크린샷 픽셀"), (Coordinates::Normalized, "0–1000 비율")];
            widgets::segmented(ui, &mut self.coordinates, &options);
            ui.label(RichText::new("모델이 클릭 위치를 주는 방식입니다. 모델 설명서를 확인하세요.").small().color(pal.text_weak));
        });
    }

    fn model_field(&mut self, ui: &mut egui::Ui) {
        let pal = palette(ui);
        let kind = self.kind();
        // The list fetched for these settings, to pick a model from.
        let base_url = self.base_url.trim().to_owned();
        let current = self
            .listing
            .as_ref()
            .filter(|(listed, url, _)| *listed == kind && (!kind.is_compatible() || *url == base_url))
            .map(|(_, _, listing)| listing.lock().unwrap_or_else(PoisonError::into_inner).clone());
        let mut load = false;
        let model = self.models.entry(kind).or_insert_with(|| kind.default_model().to_owned());
        widgets::field(ui, "모델", None, |ui| {
            let hint = if kind.is_compatible() { "서버의 모델 이름 (예: qwen3-vl)" } else { kind.default_model() };
            ui.horizontal(|ui| {
                let button_width = 96.0;
                let width = (ui.available_width() - button_width - 8.0).max(120.0);
                ui.add(widgets::text_input(ui, model, hint, width, false));
                load = ui.add_sized([button_width, 30.0], egui::Button::new("모델 불러오기")).clicked();
            });
            match current {
                Some(None) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("모델 목록을 불러오는 중…").small().color(pal.text_weak));
                    });
                }
                Some(Some(Ok(models))) if models.is_empty() => {
                    ui.label(RichText::new("서버가 모델을 알려 주지 않았습니다. 이름을 직접 입력하세요.").small().color(pal.text_weak));
                }
                Some(Some(Ok(models))) => {
                    egui::ComboBox::from_id_salt("ai_model_list")
                        .width(ui.available_width())
                        .selected_text(format!("목록에서 선택 ({}개)", models.len()))
                        .show_ui(ui, |ui| {
                            for id in &models {
                                if ui.selectable_label(model == id, id.as_str()).clicked() {
                                    model.clone_from(id);
                                }
                            }
                        });
                }
                Some(Some(Err(error))) => {
                    ui.label(RichText::new(format!("불러오지 못했습니다: {error}")).small().color(pal.danger));
                }
                None => {}
            }
            let note = match kind {
                Kind::Anthropic => {
                    "computer_toolset_20260801을 지원하는 모델이어야 합니다(Opus·Sonnet·Haiku 5.5 이후). \
                     Opus가 가장 정확하고, Haiku가 가장 빠르고 저렴합니다."
                }
                Kind::OpenAi | Kind::Gemini => "화면 조작(computer use)을 지원하는 모델이어야 합니다. 지원 모델은 제공자 문서에서 확인하세요.",
                _ => "화면을 보고 도구를 호출할 수 있는 비전 모델이어야 합니다. UI 조작용으로 학습된 모델이 정확합니다.",
            };
            ui.label(RichText::new(note).small().color(pal.text_weak));
        });
        if load {
            self.load_models(ui.ctx());
        }
    }

    fn key_field(&mut self, ui: &mut egui::Ui) {
        let pal = palette(ui);
        let kind = self.kind();
        let has_key = self.has_key(kind);
        let label = match kind {
            Kind::Anthropic => "Anthropic API 키",
            Kind::OpenAi => "OpenAI API 키",
            Kind::Gemini => "Gemini API 키",
            Kind::OpenAiCompatible | Kind::AnthropicCompatible => "API 키 (서버에 필요할 때만)",
        };
        let hint = if has_key { "저장된 키 사용 (바꾸려면 새 키 입력)" } else { "API 키" };
        let width = ui.available_width();
        let error = self.error.clone();
        widgets::field(ui, label, error.as_deref(), |ui| {
            ui.add(widgets::text_input(ui, &mut self.key, hint, width, error.is_some()).password(true));
            ui.horizontal(|ui| {
                let status = if has_key { format!("{}에 저장됨", platform::SECRET_STORE) } else { "저장된 키 없음".to_owned() };
                ui.label(RichText::new(status).small().color(pal.text_weak));
                if has_key && ui.small_button("키 삭제").clicked() {
                    match crate::secret_store::delete(&[kind.key_account().to_owned()]) {
                        Ok(()) => {
                            self.stored.insert(kind, false);
                        }
                        Err(error) => self.error = Some(format!("키를 삭제하지 못했습니다: {error:#}")),
                    }
                }
            });
        });
    }

    /// Stores the key and settings; `None` keeps the window open with an error.
    fn save(&mut self, store: &mut ProfileStore) -> Option<String> {
        let settings = self.settings();
        let kind = settings.kind;
        let key = self.key.trim().to_owned();
        if kind.is_compatible()
            && let Err(error) = provider::check_base_url(&settings.base_url)
        {
            self.error = Some(format!("{error:#}"));
            return None;
        }
        if settings.model.is_empty() && kind.default_model().is_empty() {
            self.error = Some("모델 이름을 입력하세요.".to_owned());
            return None;
        }
        if !key.is_empty() {
            if let Err(error) = crate::secret_store::set(kind.key_account(), &key) {
                self.error = Some(format!("키를 저장하지 못했습니다: {error:#}"));
                return None;
            }
            self.stored.insert(kind, true);
            self.key.clear();
        }
        if self.enabled && kind.needs_key() && !self.has_key(kind) {
            self.error = Some("AI를 켜려면 API 키를 입력하세요.".to_owned());
            return None;
        }
        if let Err(error) = store.set_ai(settings) {
            self.error = Some(format!("설정을 저장하지 못했습니다: {error:#}"));
            return None;
        }
        Some(if self.enabled { "AI 공동 작업자를 켰습니다".to_owned() } else { "AI 공동 작업자를 껐습니다".to_owned() })
    }
}
