fn main() {
    let out_dir = std::env::var_os("OUT_DIR").unwrap();
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
}
