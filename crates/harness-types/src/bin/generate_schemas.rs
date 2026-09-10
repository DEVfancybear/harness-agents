#![forbid(unsafe_code)]

use std::{error::Error, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let schema_directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas");
    fs::create_dir_all(&schema_directory)?;

    for document in harness_types::generated_schema_documents() {
        let mut rendered = serde_json::to_string_pretty(&document.value)?;
        rendered.push('\n');
        let output = schema_directory.join(document.file_name);
        fs::write(&output, rendered)?;
        println!("WROTE_SCHEMA: {}", output.display());
    }

    Ok(())
}
