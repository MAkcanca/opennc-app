fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/app.slint", config).expect("compiling ui/app.slint");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // exe icon and version info (shown in Explorer, Task Manager and the installer)
        let mut res = winresource::WindowsResource::new();
        res.set_icon("ui/icon.ico")
            .set("ProductName", "OpenNC")
            .set("FileDescription", "OpenNC noise cancellation")
            .set("LegalCopyright", "Copyright 2026 Mustafa Akcanca. Apache-2.0.");
        res.compile().expect("compiling Windows resources");
        println!("cargo:rerun-if-changed=ui/icon.ico");
    }
}
