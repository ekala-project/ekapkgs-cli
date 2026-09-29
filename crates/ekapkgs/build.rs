fn main() {
    let lib = pkg_config::Config::new()
        .atleast_version("3.0.0")
        .probe("fuse3")
        .expect("libfuse3 not found (install libfuse3-dev / fuse3 and pkg-config)");

    for path in lib.link_paths {
        if let Some(path_str) = path.to_str() {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", path_str);
        }
    }

    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
}
