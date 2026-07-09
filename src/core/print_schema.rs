use gqlrs::SDLExportOptions;
use gqlrs::dynamic::Schema;

/// SDL returned from `AsyncSchemaInner` isn't standard
/// We clean it up before returning.
#[must_use]
pub fn print_schema(schema: &Schema) -> String {
    let sdl = schema.sdl_with_options(SDLExportOptions::new().sorted_fields());
    let lines: Vec<&str> = sdl.lines().collect();

    // Mark lines to skip: directive @include/@skip and their preceding doc comments
    let mut skip = vec![false; lines.len()];
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("directive @include") || trimmed.starts_with("directive @skip") {
            skip[i] = true;
            // Walk backwards to remove the preceding """ doc comment block
            let mut j = i.wrapping_sub(1);
            while j < lines.len() {
                let prev = lines[j].trim();
                if prev == "\"\"\"" {
                    skip[j] = true;
                    // If this is the closing """, keep going to find the opening """
                    if j > 0 && skip[j] {
                        let mut k = j.wrapping_sub(1);
                        while k < lines.len() {
                            skip[k] = true;
                            if lines[k].trim() == "\"\"\"" {
                                break;
                            }
                            k = k.wrapping_sub(1);
                        }
                    }
                    break;
                }
                j = j.wrapping_sub(1);
            }
        }
    }

    let mut result = String::new();
    let mut prev_line_empty = false;

    for (i, line) in lines.iter().enumerate() {
        if skip[i] {
            continue;
        }
        let trimmed_line = line.trim();
        if trimmed_line.is_empty() {
            if !prev_line_empty {
                result.push('\n');
            }
            prev_line_empty = true;
        } else {
            let formatted_line = if line.starts_with('\t') {
                line.replace('\t', "  ")
            } else {
                line.to_string()
            };
            result.push_str(&formatted_line);
            result.push('\n');
            prev_line_empty = false;
        }
    }

    result.trim().to_string()
}
