fn main() {
    println!("cargo:rerun-if-changed=../../packaging/windows/icon.ico");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../packaging/windows/icon.ico");
        res.set("ProductName", "Sill Desktop");
        res.set("FileDescription", "Sill Desktop");
        res.compile().expect("the .exe's icon");
    }
}
