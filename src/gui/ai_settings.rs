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
    auto_approve: bool,
    /// A new key for the selected provider; empty keeps the stored one.
    key: String,
    /// Whether each provider has a key stored, looked up once.
    stored: HashMap<Kind, bool>,
    /// The model list fetched last, with the settings it was fetched for.
    listing: Option<(Kind, String, Listing)>,
    /// What is wrong, and the field it is shown under.
    error: Option<(Field, String)>,
    /// "키 삭제" was pressed once: the next press deletes.
    confirm_delete: bool,
}

/// Where an error belongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Url,
    Key,
    Model,
    General,
}

/// A settings row: an icon tile, a title with an optional caption under it, and a switch on the right.
fn switch_row(
    ui: &mut egui::Ui,
    icon: widgets::Icon,
    tile: egui::Color32,
    title: &str,
    caption: Option<&str>,
    value: &mut bool,
) {
    let pal = palette(ui);
    let size = egui::Vec2::new(ui.available_width(), 52.0);
    ui.allocate_ui_with_layout(
        size,
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            widgets::icon_tile(ui, icon, tile, 30.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                // A column fills the row's height and starts at its top: pad it to sit in the middle.
                let line = ui.text_style_height(&egui::TextStyle::Body);
                let small = ui.text_style_height(&egui::TextStyle::Small);
                let content = if caption.is_some() {
                    line + 1.0 + small
                } else {
                    line
                };
                ui.add_space(((size.y - content) / 2.0).max(0.0));
                ui.label(RichText::new(title).color(pal.text));
                if let Some(caption) = caption {
                    ui.label(RichText::new(caption).small().color(pal.text_weak));
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                widgets::toggle(ui, value);
            });
        },
    );
}

/// Orange of the "act without asking" tile, the system orange.
const ORANGE: egui::Color32 = egui::Color32::from_rgb(0xFF, 0x9F, 0x0A);

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
            auto_approve: settings.auto_approve,
            key: String::new(),
            stored: HashMap::new(),
            listing: None,
            error: None,
            confirm_delete: false,
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

    /// The window for a design screenshot: no key is looked up in the OS credential store.
    #[cfg(test)]
    pub(super) fn for_screenshot(store: &ProfileStore, server: bool) -> Self {
        let mut form = Self::open(store);
        for kind in [
            Kind::Anthropic,
            Kind::OpenAi,
            Kind::Gemini,
            Kind::OpenAiCompatible,
            Kind::AnthropicCompatible,
        ] {
            form.stored.insert(kind, kind == Kind::Anthropic);
        }
        form.enabled = true;
        if server {
            form.tab = Tab::Server;
            form.base_url = "http://localhost:11434".into();
            form.models
                .insert(Kind::OpenAiCompatible, "qwen3-vl".into());
        }
        form
    }

    /// The screenshot window with the AI off, asking whether to delete the stored key.
    #[cfg(test)]
    pub(super) fn off_and_confirming_delete(mut self) -> Self {
        self.enabled = false;
        self.confirm_delete = true;
        self
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
            auto_approve: self.auto_approve,
        }
    }

    pub(super) fn ui(&mut self, ctx: &egui::Context, store: &mut ProfileStore) -> Outcome {
        let mut outcome = Outcome::Open;
        let frame =
            egui::Frame::popup(&ctx.global_style()).inner_margin(egui::Margin::symmetric(24, 20));
        let modal = egui::Modal::new(Id::unique("ai_settings")).frame(frame).show(ctx, |ui| {
            let pal = palette(ui);
            ui.set_width(460.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                widgets::icon_tile(ui, widgets::Icon::Sparkles, pal.accent, 40.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(RichText::new("AI 공동 작업자").size(17.0).bold().color(pal.text));
                    ui.label(RichText::new("원격 데스크톱 창의 AI와 대화하며 PC를 함께 조작합니다.").small().color(pal.text_weak));
                });
            });
            ui.add_space(14.0);
            // A small window scrolls the settings; the buttons below stay in view.
            let max_height = (ctx.content_rect().height() - OUTSIDE_SCROLL).max(160.0);
            let scrolled = egui::ScrollArea::vertical().max_height(max_height).auto_shrink([false, true]).show(ui, |ui| {
                // The two switches, grouped like a settings list.
                egui::Frame::new().fill(pal.bg).corner_radius(egui::CornerRadius::same(12)).inner_margin(egui::Margin::symmetric(14, 4)).show(
                    ui,
                    |ui| {
                        ui.set_width(ui.available_width());
                        switch_row(ui, widgets::Icon::Monitor, pal.accent, "RDP 세션에서 AI 사용", None, &mut self.enabled);
                        ui.add(egui::Separator::default().spacing(0.0));
                        // Acting by itself means nothing while the AI is off.
                        ui.add_enabled_ui(self.enabled, |ui| {
                            switch_row(ui, widgets::Icon::Zap, ORANGE, "자동 실행", Some("거절하지 않으면 3초 뒤 실행합니다"), &mut self.auto_approve);
                        });
                    },
                );
                ui.add_space(8.0);
                egui::Frame::new().fill(pal.accent_soft).corner_radius(egui::CornerRadius::same(10)).inner_margin(egui::Margin::symmetric(12, 10)).show(
                    ui,
                    |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            ui.add(widgets::Icon::Shield.image(widgets::ICON_MD, pal.accent));
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!(
                                    "켜면 원격 화면이 {}로 전송됩니다. AI의 동작은 실행하기 전에 먼저 보여 드리며, 자동 실행도 Esc로 막을 수 있습니다. \
                                     직접 조작 중이거나 안전 확인이 필요한 동작은 자동 실행하지 않습니다.",
                                    provider::destination(&self.settings())
                                ))
                                    .small()
                                    .color(pal.text),
                                )
                                .wrap(),
                            );
                        });
                    },
                );
                ui.add_space(12.0);

                widgets::section_title(ui, widgets::Icon::Server, pal.accent, "제공자·모델", None);
                // Provider names stay text: no logos.
                let tabs = [(Tab::Claude, "Claude"), (Tab::Gpt, "GPT"), (Tab::Gemini, "Gemini"), (Tab::Server, "내 서버")];
                if widgets::segmented(ui, &mut self.tab, &tabs) {
                    self.key.clear();
                    self.error = None;
                    self.confirm_delete = false;
                }
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
                    ui.add_space(6.0);
                    egui::CollapsingHeader::new(RichText::new("고급").color(pal.text_weak)).id_salt("ai_advanced").show(ui, |ui| {
                        self.coordinates_field(ui);
                    });
                }
                if let Some((Field::General, error)) = &self.error {
                    ui.add_space(6.0);
                    ui.label(RichText::new(error).small().color(pal.danger));
                }
                ui.add_space(4.0);
                ui.label(RichText::new("바꾼 설정은 새로 여는 세션부터 적용됩니다.").small().color(pal.text_weak));
            });
            // A hairline over the buttons while more settings are hidden below it.
            let hidden_below = scrolled.content_size.y - scrolled.state.clamped_offset().y - scrolled.inner_rect.height();
            if hidden_below > 1.0 {
                let edge = scrolled.inner_rect;
                ui.painter().hline(edge.x_range(), edge.bottom(), egui::Stroke::new(1.0, pal.border));
            }
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

    /// The error to show under `field`.
    fn error_for(&self, field: Field) -> Option<String> {
        self.error
            .as_ref()
            .filter(|(at, _)| *at == field)
            .map(|(_, error)| error.clone())
    }

    fn server_fields(&mut self, ui: &mut egui::Ui) {
        let pal = palette(ui);
        widgets::field(ui, "API 형식", None, |ui| {
            let formats = [(Kind::OpenAiCompatible, "OpenAI 형식"), (Kind::AnthropicCompatible, "Claude 형식")];
            widgets::segmented(ui, &mut self.server_kind, &formats);
            ui.add_space(2.0);
            ui.label(RichText::new("Ollama, vLLM, LM Studio 등은 대개 OpenAI 형식입니다.").small().color(pal.text_weak));
        });
        ui.add_space(10.0);
        let width = ui.available_width();
        let url_error = self.error_for(Field::Url);
        widgets::field(
            ui,
            "서버 주소",
            url_error.as_deref(),
            |ui| {
                ui.add(
                    widgets::text_input(ui, &mut self.base_url, "http://localhost:11434", width, url_error.is_some())
                        .leading_icon(widgets::Icon::Globe),
                );
                ui.label(
                    RichText::new("https 주소, 또는 이 기기·사내망의 http 주소")
                        .small()
                        .color(pal.text_weak),
                );
            },
        );
    }

    fn coordinates_field(&mut self, ui: &mut egui::Ui) {
        let pal = palette(ui);
        widgets::field(ui, "좌표 방식", None, |ui| {
            let options = [(Coordinates::Pixels, "스크린샷 픽셀"), (Coordinates::Normalized, "0–1000 비율")];
            widgets::segmented(ui, &mut self.coordinates, &options);
            ui.add_space(2.0);
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
        let model_error = self.error_for(Field::Model);
        let model = self
            .models
            .entry(kind)
            .or_insert_with(|| kind.default_model().to_owned());
        widgets::field(
            ui,
            "모델",
            model_error.as_deref(),
            |ui| {
                let hint = if kind.is_compatible() {
                    "서버의 모델 이름 (예: qwen3-vl)"
                } else {
                    kind.default_model()
                };
                ui.horizontal(|ui| {
                    // The input and the button together fill the row, like the inputs above.
                    let button_width = 132.0;
                    let width = (ui.available_width() - button_width - ui.spacing().item_spacing.x).max(120.0);
                    ui.add(widgets::text_input(ui, model, hint, width, false).leading_icon(widgets::Icon::Cpu));
                    load = ui
                        .add_sized(
                            [button_width, 32.0],
                            widgets::labeled_button(
                                widgets::Icon::Refresh,
                                "모델 불러오기",
                                pal.text,
                            ),
                        )
                        .clicked();
                });
                match current {
                    Some(None) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(
                                RichText::new("모델 목록을 불러오는 중…")
                                    .small()
                                    .color(pal.text_weak),
                            );
                        });
                    }
                    Some(Some(Ok(models))) if models.is_empty() => {
                        ui.label(
                            RichText::new(
                                "서버가 모델을 알려 주지 않았습니다. 이름을 직접 입력하세요.",
                            )
                            .small()
                            .color(pal.text_weak),
                        );
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
                        ui.label(
                            RichText::new(format!("불러오지 못했습니다: {error}"))
                                .small()
                                .color(pal.danger),
                        );
                    }
                    None => {}
                }
                let note = match kind {
                    Kind::Anthropic => {
                        "computer_toolset_20260801을 지원하는 모델이어야 합니다(Opus·Sonnet·Haiku 5.5 이후). \
                     Opus가 가장 정확하고, Haiku가 가장 빠르고 저렴합니다."
                    }
                    Kind::OpenAi | Kind::Gemini => {
                        "화면 조작(computer use)을 지원하는 모델이어야 합니다. 지원 모델은 제공자 문서에서 확인하세요."
                    }
                    _ => {
                        "화면을 보고 도구를 호출할 수 있는 비전 모델이어야 합니다. UI 조작용으로 학습된 모델이 정확합니다."
                    }
                };
                ui.add(egui::Label::new(RichText::new(note).small().color(pal.text_weak)).wrap());
            },
        );
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
        let error = self.error_for(Field::Key);
        widgets::field(ui, label, error.as_deref(), |ui| {
            ui.add(
                widgets::text_input(ui, &mut self.key, hint, width, error.is_some()).password(true).leading_icon(widgets::Icon::Key),
            );
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if has_key {
                    ui.add(widgets::Icon::Lock.image(13.0, pal.success));
                }
                let status = if has_key {
                    format!("{}에 저장됨", platform::SECRET_STORE)
                } else {
                    "저장된 키 없음".to_owned()
                };
                if !has_key {
                    ui.label(RichText::new(status).small().color(pal.text_weak));
                    return;
                }
                if !self.confirm_delete {
                    ui.label(RichText::new(status).small().color(pal.text_weak));
                    let delete = widgets::labeled_button(widgets::Icon::Trash, RichText::new("키 삭제").small(), pal.danger).frame(false);
                    self.confirm_delete = ui.add(delete).clicked();
                    return;
                }
                // Deleting cannot be undone, so it asks once, in place.
                ui.label(
                    RichText::new(format!("{}에서 이 키를 삭제할까요? 되돌릴 수 없습니다.", platform::SECRET_STORE))
                        .small()
                        .color(pal.text),
                );
                ui.add_space(6.0);
                let delete = widgets::labeled_button(widgets::Icon::Trash, RichText::new("삭제").small().strong(), pal.danger).frame(false);
                if ui.add(delete).clicked() {
                    self.confirm_delete = false;
                    match crate::secret_store::delete(&[kind.key_account().to_owned()]) {
                        Ok(()) => {
                            self.stored.insert(kind, false);
                        }
                        Err(error) => {
                            self.error =
                                Some((Field::Key, format!("키를 삭제하지 못했습니다: {error:#}")))
                        }
                    }
                }
                // Not "취소", which would read as "삭제 취소" right after "삭제".
                ui.add_space(6.0);
                if ui.add(egui::Button::new(RichText::new("그대로 두기").small().color(pal.text_weak)).frame(false)).clicked() {
                    self.confirm_delete = false;
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
            self.error = Some((Field::Url, format!("{error:#}")));
            return None;
        }
        if settings.model.is_empty() && kind.default_model().is_empty() {
            self.error = Some((Field::Model, "모델 이름을 입력하세요.".to_owned()));
            return None;
        }
        if !key.is_empty() {
            if let Err(error) = crate::secret_store::set(kind.key_account(), &key) {
                self.error = Some((Field::Key, format!("키를 저장하지 못했습니다: {error:#}")));
                return None;
            }
            self.stored.insert(kind, true);
            self.key.clear();
        }
        if self.enabled && kind.needs_key() && !self.has_key(kind) {
            self.error = Some((Field::Key, "AI를 켜려면 API 키를 입력하세요.".to_owned()));
            return None;
        }
        if let Err(error) = store.set_ai(settings) {
            self.error = Some((
                Field::General,
                format!("설정을 저장하지 못했습니다: {error:#}"),
            ));
            return None;
        }
        Some(if self.enabled { "AI 공동 작업자를 켰습니다".to_owned() } else { "AI 공동 작업자를 껐습니다".to_owned() })
    }
}
