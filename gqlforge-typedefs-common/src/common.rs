use gqlrs::{Pos, Positioned};
use serde_json::{Map, Value};

pub fn get_description(schema: &Map<String, Value>) -> Option<&str> {
    schema.get("description").and_then(Value::as_str)
}

pub fn first_char_to_upper(name: &mut String) {
    if let Some(first_char) = name.chars().next() {
        // Remove the first character and make it uppercase
        let first_char_upper = first_char.to_uppercase().to_string();

        // Remove the first character from the original string
        let mut chars = name.chars();
        chars.next();

        // Replace the original string with the new one
        *name = first_char_upper + chars.as_str();
    }
}

pub fn first_char_to_lower(name: &str) -> String {
    if let Some(first_char) = name.chars().next() {
        // Remove the first character and make it uppercase
        let first_char_upper = first_char.to_lowercase().to_string();

        // Remove the first character from the original string
        let mut chars = name.chars();
        chars.next();

        return format!("{}{}", first_char_upper, chars.collect::<String>());
    }

    String::new()
}

pub fn pos<A>(a: A) -> Positioned<A> {
    Positioned::new(a, Pos::default())
}
