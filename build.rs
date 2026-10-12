//! 构建脚本：将 Winmato.ico 嵌入可执行文件（Windows 资源）。
//!
//! 仅在 Windows 目标上执行；非 Windows 平台跳过（资源嵌入无意义）。
fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let mut res = winres::WindowsResource::new();
        res.set_icon("Winmato.ico");
        if let Err(e) = res.compile() {
            // 图标嵌入失败不阻断构建——exe 仍可运行，只是无嵌入式图标
            eprintln!("warning: failed to embed Winmato.ico: {e}");
        }
    }
}
