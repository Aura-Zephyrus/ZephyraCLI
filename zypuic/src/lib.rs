//! `.zypui` CLI 层：转发主仓的 zypui-compile 库（含排版判据）。
pub use zypui_compile::{
    compile, format_ui, has_errors, is_formatted, parse, parse_all, Diag, Level, XEl, XNode,
};
