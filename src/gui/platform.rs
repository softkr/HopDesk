//! Wording and fonts that differ per operating system.

/// Where passwords are kept, e.g. "macOS 키체인에 저장됩니다".
pub const SECRET_STORE: &str = if cfg!(target_os = "macos") {
    "macOS 키체인"
} else if cfg!(windows) {
    "Windows 자격 증명 관리자"
} else {
    "시스템 키링"
};

/// Short form for tight spots, e.g. the "키체인에 저장됨" pill.
pub const SECRET_STORE_SHORT: &str = if cfg!(target_os = "macos") {
    "키체인"
} else if cfg!(windows) {
    "자격 증명 관리자"
} else {
    "키링"
};

/// Name of the local machine in the route diagram.
pub const THIS_DEVICE: &str = if cfg!(target_os = "macos") { "이 Mac" } else { "이 PC" };

/// Shortcut label for the command modifier (egui's `Modifiers::COMMAND`): "⌘N" or "Ctrl+N".
pub fn shortcut(key: &str) -> String {
    if cfg!(target_os = "macos") { format!("⌘{key}") } else { format!("Ctrl+{key}") }
}

/// A UI font with matching regular and bold faces that covers Hangul.
pub struct UiFont {
    pub regular: (std::path::PathBuf, u32),
    pub bold: (std::path::PathBuf, u32),
}

/// First installed candidate; `None` falls back to egui's bundled font (plus system fallback).
pub fn ui_font() -> Option<UiFont> {
    use std::path::PathBuf;
    let mut candidates: Vec<((PathBuf, u32), (PathBuf, u32))> = Vec::new();
    if cfg!(target_os = "macos") {
        let sd = PathBuf::from("/System/Library/Fonts/AppleSDGothicNeo.ttc");
        candidates.push(((sd.clone(), 0), (sd, 4)));
    } else if cfg!(windows) {
        let dir = PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into())).join("Fonts");
        candidates.push(((dir.join("malgun.ttf"), 0), (dir.join("malgunbd.ttf"), 0)));
    } else {
        // Noto Sans CJK collections hold JP, KR, SC, TC, HK in that order; face 1 is Korean.
        for dir in [
            "/usr/share/fonts/opentype/noto",
            "/usr/share/fonts/noto-cjk",
            "/usr/share/fonts/google-noto-cjk",
            "/usr/share/fonts/opentype/noto-cjk",
        ] {
            let dir = PathBuf::from(dir);
            candidates.push(((dir.join("NotoSansCJK-Regular.ttc"), 1), (dir.join("NotoSansCJK-Bold.ttc"), 1)));
        }
        let nanum = PathBuf::from("/usr/share/fonts/truetype/nanum");
        candidates.push(((nanum.join("NanumGothic.ttf"), 0), (nanum.join("NanumGothicBold.ttf"), 0)));
    }
    candidates
        .into_iter()
        .find(|(regular, _)| regular.0.is_file())
        .map(|(regular, bold)| {
            let bold = if bold.0.is_file() { bold } else { regular.clone() };
            UiFont { regular, bold }
        })
}
