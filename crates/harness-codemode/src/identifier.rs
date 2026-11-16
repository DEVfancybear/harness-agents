//! The identifier a script uses for a tool (pi's `toCodemodeIdentifier`):
//! characters that are not valid in a JavaScript identifier become `_`, so
//! `my-tool` is `tools.my_tool` as well as `tools["my-tool"]`.

#[must_use]
pub fn to_identifier(name: &str) -> String {
    let mut identifier = String::with_capacity(name.len());
    for c in name.chars() {
        let valid = if identifier.is_empty() {
            c.is_ascii_alphabetic() || c == '_' || c == '$'
        } else {
            c.is_ascii_alphanumeric() || c == '_' || c == '$'
        };
        identifier.push(if valid { c } else { '_' });
    }
    if identifier.is_empty() {
        "_".to_owned()
    } else {
        identifier
    }
}

#[cfg(test)]
mod tests {
    use super::to_identifier;

    #[test]
    fn replaces_what_an_identifier_cannot_hold() {
        assert_eq!(to_identifier("mcp__docs__search"), "mcp__docs__search");
        assert_eq!(to_identifier("my-tool"), "my_tool");
        assert_eq!(to_identifier("9lives"), "_lives");
        assert_eq!(to_identifier("a.b c"), "a_b_c");
        assert_eq!(to_identifier(""), "_");
    }
}
