use std::io::Write;

fn main() {
    let out_dir = std::env::var_os("OUT_DIR").unwrap();
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR").unwrap();

    // Make a `SyntaxSetBuilder` with Rhai syntax and write it to `syntax.bin`
    println!("cargo::rerun-if-changed=syntax");
    let dest_path = std::path::Path::new(&out_dir).join("syntax.bin");
    let mut dest_file = std::fs::File::create(&dest_path).unwrap();

    let mut builder = syntect::parsing::SyntaxSetBuilder::new();
    builder.add_from_folder("syntax", true).unwrap();
    let ps = builder.build();
    bincode::serde::encode_into_std_write(
        &ps,
        &mut dest_file,
        bincode::config::standard(),
    )
    .unwrap();

    // Add all of our examples into `examples.rs`
    println!("cargo::rerun-if-changed=examples");
    let dest_path = std::path::Path::new(&out_dir).join("examples.rs");
    let mut dest_file = std::fs::File::create(&dest_path).unwrap();
    writeln!(&mut dest_file, "pub const EXAMPLES: &[(&str, &str)] = &[")
        .unwrap();
    let paths =
        std::fs::read_dir(std::path::Path::new(&manifest_dir).join("examples"))
            .unwrap();
    for d in paths {
        let path = d.unwrap().path();
        if path.extension().is_some_and(|o| o == "half") {
            let f = std::fs::read_to_string(&path).unwrap();
            writeln!(
                &mut dest_file,
                "    (\"{}\", {f:?}),",
                path.file_name().unwrap().to_str().unwrap(),
            )
            .unwrap();
        }
    }
    writeln!(&mut dest_file, "];").unwrap();
}
