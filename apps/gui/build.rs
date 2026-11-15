fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=assets/ruscello.ico");

    #[cfg(windows)]
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        winresource::WindowsResource::new()
            .set_icon("assets/ruscello.ico")
            .set("ProductName", "Ruscello Download Manager")
            .set("FileDescription", "Ruscello Download Manager")
            .compile()?;
    }
    Ok(())
}
