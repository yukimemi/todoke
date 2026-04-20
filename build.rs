//! Windows-only: embed the application icon and version info into the .exe so
//! the binary looks right when pinned to the taskbar, shown in Explorer, or
//! registered as the default program for a file type.

fn main() {
    #[cfg(target_os = "windows")]
    {
        if let Err(e) = winresource::WindowsResource::new()
            .set_icon("assets/icon.ico")
            .compile()
        {
            // Don't fail the build if resource compilation fails (e.g. missing
            // toolchain on a minimal Windows dev env). Icon is cosmetic; the
            // binary still works.
            println!("cargo:warning=failed to embed Windows resources: {e}");
        }
    }
}
