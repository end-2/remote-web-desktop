fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        // ScreenCaptureKit's Swift bridge links Swift concurrency. Dependency
        // rustc-link-arg directives do not propagate to this executable, so
        // declare the OS runtime search path at the final binary/test target.
        // macOS ships this runtime in its dyld shared cache; no Xcode path or
        // copied runtime library should be required on the destination Mac.
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
        println!("cargo:rustc-link-arg=-Wl,-weak_framework,ScreenCaptureKit");
    }
}
