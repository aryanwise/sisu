use tree_sitter::{Language, Node, Parser, Point};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AstSlice {
    pub start_line: usize, // 1-indexed
    pub end_line: usize,   // 1-indexed, inclusive
    pub content: String,
}

impl AstSlice {
    pub fn format_numbered(&self) -> String {
        self.content
            .lines()
            .enumerate()
            .map(|(i, line)| format!("{:>4} | {line}", self.start_line + i))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn load_language(language: &str) -> Result<Language, String> {
    match language.to_lowercase().as_str() {
        "python" | "py" => Ok(tree_sitter_python::LANGUAGE.into()),
        "rust" | "rs" => Ok(tree_sitter_rust::LANGUAGE.into()),
        "typescript" | "ts" => Ok(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        other => Err(format!("No Tree-sitter grammar for language '{other}'")),
    }
}

fn parse_source(language: &str, source: &str) -> Result<tree_sitter::Tree, String> {
    let ts_lang = load_language(language)?;
    let mut parser = Parser::new();
    parser
        .set_language(&ts_lang)
        .map_err(|e| format!("Failed to set Tree-sitter language '{language}': {e}"))?;

    parser
        .parse(source, None)
        .ok_or_else(|| format!("Tree-sitter failed to parse '{language}' source"))
}

/// Extracts compact public/top-level signatures (stripping function bodies)
/// so subsequent milestones see exact types & function headers under a tight token budget.
pub fn extract_signatures(language: &str, source: &str) -> Result<String, String> {
    let tree = parse_source(language, source)?;
    let root = tree.root_node();

    let out = match language.to_lowercase().as_str() {
        "python" | "py" => extract_python_signatures(root, source),
        "rust" | "rs" => extract_rust_signatures(root, source),
        "typescript" | "ts" => extract_ts_signatures(root, source),
        other => return Err(format!("Unsupported signature extraction for '{other}'")),
    };

    Ok(out)
}

/// Finds the smallest enclosing function, method, or class containing `target_line` (1-indexed).
/// Slices whole lines so leading indentation stays intact for verbatim surgical patches.
pub fn extract_enclosing_node(
    language: &str,
    source: &str,
    target_line: usize,
) -> Result<Option<AstSlice>, String> {
    let lines: Vec<&str> = source.lines().collect();
    if lines.is_empty() || target_line == 0 {
        return Ok(None);
    }

    let tree = parse_source(language, source)?;
    let row = target_line.saturating_sub(1).min(lines.len() - 1);
    let line_text = lines[row];
    // Land on the first non-whitespace byte so indented statements hit the inner node
    let col = line_text.len() - line_text.trim_start().len();
    let point = Point::new(row, col);

    let Some(mut node) = tree.root_node().descendant_for_point_range(point, point) else {
        return Ok(None);
    };

    let lang_key = language.to_lowercase();
    let enclosing = loop {
        if is_enclosing_scope(&lang_key, node.kind()) {
            // In Python, include decorators (`@app.get`, `@field_validator`) wrapping the def/class
            if lang_key == "python" || lang_key == "py" {
                if let Some(parent) = node.parent() {
                    if parent.kind() == "decorated_definition" {
                        break Some(parent);
                    }
                }
            }
            break Some(node);
        }

        match node.parent() {
            Some(parent) => node = parent,
            None => break None,
        }
    };

    let Some(target_node) = enclosing else {
        return Ok(None);
    };

    let start_row = target_node.start_position().row.min(lines.len() - 1);
    let end_row = target_node.end_position().row.min(lines.len() - 1);

    Ok(Some(AstSlice {
        start_line: start_row + 1,
        end_line: end_row + 1,
        content: lines[start_row..=end_row].join("\n"),
    }))
}

fn is_enclosing_scope(lang: &str, kind: &str) -> bool {
    match lang {
        "python" | "py" => matches!(
            kind,
            "function_definition" | "class_definition" | "decorated_definition"
        ),
        "rust" | "rs" => matches!(
            kind,
            "function_item" | "impl_item" | "struct_item" | "enum_item" | "trait_item"
        ),
        "typescript" | "ts" => matches!(
            kind,
            "method_definition"
                | "function_declaration"
                | "class_declaration"
                | "interface_declaration"
        ),
        _ => false,
    }
}

fn slice_header_before_body<'a>(outer: Node, inner_with_body: Node, source: &'a str) -> &'a str {
    if let Some(body) = inner_with_body.child_by_field_name("body") {
        source[outer.start_byte()..body.start_byte()].trim_end()
    } else {
        source[outer.start_byte()..outer.end_byte()].trim_end()
    }
}

// --- Python Signature Extractor ---
fn extract_python_signatures(root: Node, source: &str) -> String {
    let mut parts = Vec::new();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        match child.kind() {
            "import_statement" | "import_from_statement" => {
                parts.push(
                    source[child.start_byte()..child.end_byte()]
                        .trim()
                        .to_string(),
                );
            }
            "function_definition" => {
                let header = slice_header_before_body(child, child, source);
                parts.push(format!("{header} ..."));
            }
            "class_definition" => {
                parts.push(format_python_class(child, child, source));
            }
            "decorated_definition" => {
                if let Some(def) = child.child_by_field_name("definition") {
                    match def.kind() {
                        "function_definition" => {
                            let header = slice_header_before_body(child, def, source);
                            parts.push(format!("{header} ..."));
                        }
                        "class_definition" => {
                            parts.push(format_python_class(child, def, source));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    parts.join("\n\n")
}

fn format_python_class(outer: Node, class_node: Node, source: &str) -> String {
    let header = slice_header_before_body(outer, class_node, source);
    let mut lines = vec![header.to_string()];

    if let Some(body) = class_node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for member in body.children(&mut cursor) {
            match member.kind() {
                // Keep class/Pydantic/dataclass fields like `name: str = ""`
                "expression_statement" => {
                    let text = source[member.start_byte()..member.end_byte()].trim();
                    if !text.starts_with('"') && !text.starts_with('\'') && text.len() <= 160 {
                        lines.push(format!("    {text}"));
                    }
                }
                "function_definition" => {
                    let sig = slice_header_before_body(member, member, source);
                    lines.push(format!("    {sig} ..."));
                }
                "decorated_definition" => {
                    if let Some(def) = member.child_by_field_name("definition") {
                        if def.kind() == "function_definition" {
                            let sig = slice_header_before_body(member, def, source);
                            for l in sig.lines() {
                                lines.push(format!("    {}", l.trim_start()));
                            }
                            if let Some(last) = lines.last_mut() {
                                last.push_str(" ...");
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    if lines.len() == 1 {
        lines.push("    ...".to_string());
    }

    lines.join("\n")
}

// --- Rust Signature Extractor ---
fn extract_rust_signatures(root: Node, source: &str) -> String {
    let mut parts = Vec::new();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        match child.kind() {
            "use_declaration" | "struct_item" | "enum_item" | "type_item" | "const_item" => {
                parts.push(
                    source[child.start_byte()..child.end_byte()]
                        .trim()
                        .to_string(),
                );
            }
            "function_item" => {
                let header = slice_header_before_body(child, child, source);
                parts.push(format!("{header} {{ ... }}"));
            }
            "impl_item" | "trait_item" => {
                let header = slice_header_before_body(child, child, source);
                let mut block = vec![format!("{header} {{")];

                if let Some(body) = child.child_by_field_name("body") {
                    let mut body_cursor = body.walk();
                    for item in body.children(&mut body_cursor) {
                        match item.kind() {
                            "function_item" => {
                                let sig = slice_header_before_body(item, item, source);
                                block.push(format!("    {sig} {{ ... }}"));
                            }
                            "function_signature_item" => {
                                let sig = source[item.start_byte()..item.end_byte()].trim();
                                block.push(format!("    {sig}"));
                            }
                            _ => {}
                        }
                    }
                }

                block.push("}".to_string());
                parts.push(block.join("\n"));
            }
            _ => {}
        }
    }

    parts.join("\n\n")
}

// --- TypeScript Signature Extractor ---
fn extract_ts_signatures(root: Node, source: &str) -> String {
    let mut parts = Vec::new();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        let (outer, inner) = if child.kind() == "export_statement" {
            match child.child_by_field_name("declaration") {
                Some(decl) => (child, decl),
                None => continue,
            }
        } else {
            (child, child)
        };

        match inner.kind() {
            "import_statement"
            | "interface_declaration"
            | "type_alias_declaration"
            | "enum_declaration" => {
                parts.push(
                    source[outer.start_byte()..outer.end_byte()]
                        .trim()
                        .to_string(),
                );
            }
            "function_declaration" => {
                let header = slice_header_before_body(outer, inner, source);
                parts.push(format!("{header} {{ ... }}"));
            }
            "class_declaration" => {
                let header = slice_header_before_body(outer, inner, source);
                let mut block = vec![format!("{header} {{")];

                if let Some(body) = inner.child_by_field_name("body") {
                    let mut body_cursor = body.walk();
                    for member in body.children(&mut body_cursor) {
                        match member.kind() {
                            "public_field_definition" => {
                                let field = source[member.start_byte()..member.end_byte()].trim();
                                block.push(format!("    {field}"));
                            }
                            "method_definition" => {
                                let sig = slice_header_before_body(member, member, source);
                                block.push(format!("    {sig} {{ ... }}"));
                            }
                            _ => {}
                        }
                    }
                }

                block.push("}".to_string());
                parts.push(block.join("\n"));
            }
            _ => {}
        }
    }

    parts.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_python_signatures_with_fields_and_strips_bodies() {
        let src = r#"
from pydantic import BaseModel

class User(BaseModel):
    id: int
    email: str

    def display_name(self) -> str:
        secret = "internal_implementation_detail"
        return f"{self.id}:{self.email}:{secret}"

def compute_score(x: float, y: float) -> float:
    temp = x * 42.0
    return temp + y
"#;

        let sigs = extract_signatures("python", src).unwrap();
        assert!(sigs.contains("class User(BaseModel):"));
        assert!(sigs.contains("id: int"));
        assert!(sigs.contains("def display_name(self) -> str: ..."));
        assert!(sigs.contains("def compute_score(x: float, y: float) -> float: ..."));
        assert!(!sigs.contains("internal_implementation_detail"));
        assert!(!sigs.contains("x * 42.0"));
    }

    #[test]
    fn extracts_enclosing_python_function_with_decorator() {
        let src = "\
def untouched():
    return 1

@staticmethod
def buggy_add(a: int, b: int) -> int:
    result = a - b
    return result

def later():
    return 2
";

        // Line 6 is `    result = a - b`
        let slice = extract_enclosing_node("python", src, 6).unwrap().unwrap();
        assert_eq!(slice.start_line, 4);
        assert_eq!(slice.end_line, 7);
        assert!(slice.content.contains("@staticmethod"));
        assert!(slice.content.contains("result = a - b"));
        assert!(!slice.content.contains("untouched"));
        assert!(!slice.content.contains("later"));
    }

    #[test]
    fn extracts_rust_signatures_and_enclosing_fn() {
        let src = r#"
pub struct Config {
    pub timeout_ms: u64,
}

impl Config {
    pub fn is_fast(&self) -> bool {
        let limit = 500;
        self.timeout_ms < limit
    }
}
"#;

        let sigs = extract_signatures("rust", src).unwrap();
        assert!(sigs.contains("pub struct Config"));
        assert!(sigs.contains("pub fn is_fast(&self) -> bool { ... }"));
        assert!(!sigs.contains("let limit = 500;"));

        // Line 9 is `        self.timeout_ms < limit`
        let slice = extract_enclosing_node("rust", src, 9).unwrap().unwrap();
        assert!(slice.content.contains("pub fn is_fast"));
        assert!(slice.content.contains("let limit = 500;"));
        assert!(!slice.content.contains("pub struct Config"));
    }

    #[test]
    fn extracts_typescript_signatures() {
        let src = r#"
export interface Task {
    id: string;
    done: boolean;
}

export class TaskStore {
    private items: Task[] = [];

    add(task: Task): void {
        this.items.push(task);
    }
}
"#;

        let sigs = extract_signatures("typescript", src).unwrap();
        assert!(sigs.contains("export interface Task"));
        assert!(sigs.contains("export class TaskStore"));
        assert!(sigs.contains("add(task: Task): void { ... }"));
        assert!(!sigs.contains("this.items.push"));
    }
}
