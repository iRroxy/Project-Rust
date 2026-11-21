use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};

/// Represents an extracted code block inside Markdown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeBlock {
    pub language: Option<String>,
    pub code: String,
}

/// Represents a structural section in the parsed Markdown document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkdownSection {
    pub level: u8,
    pub title: String,
    pub breadcrumbs: Vec<String>,
    pub content: String,
    pub code_blocks: Vec<CodeBlock>,
}

/// A fully parsed Markdown document with structural hierarchy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedMarkdown {
    pub title: Option<String>,
    pub sections: Vec<MarkdownSection>,
    pub total_chars: usize,
}

pub struct MarkdownParser;

impl MarkdownParser {
    /// Parse a raw Markdown string into structured sections and code blocks.
    pub fn parse(input: &str) -> ParsedMarkdown {
        let mut options = Options::empty();
        options.insert(Options::ENABLE_TABLES);
        options.insert(Options::ENABLE_FOOTNOTES);
        options.insert(Options::ENABLE_STRIKETHROUGH);
        options.insert(Options::ENABLE_TASKLISTS);

        let parser = Parser::new_ext(input, options);

        let mut sections: Vec<MarkdownSection> = Vec::new();
        let mut doc_title: Option<String> = None;

        // Current active section state
        let mut current_level: u8 = 0;
        let mut current_title = String::new();
        let mut current_content = String::new();
        let mut current_code_blocks = Vec::new();

        // Stack of heading titles: (level, title)
        let mut heading_stack: Vec<(u8, String)> = Vec::new();

        // Parsing flags
        let mut in_heading = false;
        let mut active_heading_level: u8 = 0;
        let mut heading_text_buffer = String::new();

        let mut in_code_block = false;
        let mut code_lang: Option<String> = None;
        let mut code_text_buffer = String::new();

        for event in parser {
            match event {
                Event::Start(Tag::Heading { level, .. }) => {
                    in_heading = true;
                    active_heading_level = heading_level_to_u8(level);
                    heading_text_buffer.clear();
                }
                Event::End(TagEnd::Heading(_)) => {
                    in_heading = false;
                    let heading_text = heading_text_buffer.trim().to_string();

                    // Flush the prior section if it has a title, text, or code blocks
                    if !current_title.is_empty()
                        || !current_content.trim().is_empty()
                        || !current_code_blocks.is_empty()
                    {
                        let breadcrumbs: Vec<String> =
                            heading_stack.iter().map(|(_, t)| t.clone()).collect();
                        sections.push(MarkdownSection {
                            level: current_level,
                            title: current_title.clone(),
                            breadcrumbs,
                            content: current_content.trim().to_string(),
                            code_blocks: std::mem::take(&mut current_code_blocks),
                        });
                        current_content.clear();
                    }

                    // Update breadcrumbs stack: pop headings with level >= current heading level
                    while let Some((lvl, _)) = heading_stack.last() {
                        if *lvl >= active_heading_level {
                            heading_stack.pop();
                        } else {
                            break;
                        }
                    }
                    heading_stack.push((active_heading_level, heading_text.clone()));

                    if doc_title.is_none() && active_heading_level == 1 {
                        doc_title = Some(heading_text.clone());
                    }

                    current_level = active_heading_level;
                    current_title = heading_text;
                }
                Event::Start(Tag::CodeBlock(kind)) => {
                    in_code_block = true;
                    code_text_buffer.clear();
                    code_lang = match kind {
                        CodeBlockKind::Fenced(lang) => {
                            let s = lang.trim().to_string();
                            if s.is_empty() { None } else { Some(s) }
                        }
                        CodeBlockKind::Indented => None,
                    };
                }
                Event::End(TagEnd::CodeBlock) => {
                    in_code_block = false;
                    let code = code_text_buffer.trim_end().to_string();
                    let lang_label = code_lang.clone().unwrap_or_default();
                    current_code_blocks.push(CodeBlock {
                        language: code_lang.take(),
                        code: code.clone(),
                    });
                    // Preserve code block inside section content for downstream chunking
                    current_content.push_str("\n```");
                    current_content.push_str(&lang_label);
                    current_content.push('\n');
                    current_content.push_str(&code);
                    current_content.push_str("\n```\n\n");
                    code_text_buffer.clear();
                }
                Event::Start(Tag::Item) if !in_heading && !in_code_block => {
                    current_content.push_str("- ");
                }
                Event::End(TagEnd::Item) if !in_heading && !in_code_block => {
                    current_content.push('\n');
                }
                Event::End(TagEnd::Paragraph) if !in_heading && !in_code_block => {
                    current_content.push_str("\n\n");
                }
                Event::Text(text) => {
                    if in_heading {
                        heading_text_buffer.push_str(&text);
                    } else if in_code_block {
                        code_text_buffer.push_str(&text);
                    } else {
                        current_content.push_str(&text);
                    }
                }
                Event::Code(inline_code) => {
                    if in_heading {
                        heading_text_buffer.push('`');
                        heading_text_buffer.push_str(&inline_code);
                        heading_text_buffer.push('`');
                    } else if in_code_block {
                        code_text_buffer.push_str(&inline_code);
                    } else {
                        current_content.push('`');
                        current_content.push_str(&inline_code);
                        current_content.push('`');
                    }
                }
                Event::SoftBreak | Event::HardBreak if !in_heading && !in_code_block => {
                    current_content.push('\n');
                }
                _ => {}
            }
        }

        // Flush trailing section if any
        if !current_title.is_empty()
            || !current_content.trim().is_empty()
            || !current_code_blocks.is_empty()
        {
            let breadcrumbs: Vec<String> = heading_stack.iter().map(|(_, t)| t.clone()).collect();
            sections.push(MarkdownSection {
                level: current_level,
                title: current_title,
                breadcrumbs,
                content: current_content.trim().to_string(),
                code_blocks: current_code_blocks,
            });
        }

        let total_chars = input.chars().count();

        ParsedMarkdown {
            title: doc_title,
            sections,
            total_chars,
        }
    }
}

fn heading_level_to_u8(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_markdown() {
        let md = r#"# Main Document Title

This is an introduction paragraph.

Second intro paragraph.

## Section 1: Overview
Here is some overview text with `inline_code`.

- Item A
- Item B

### Subsection 1.1: Deep Dive
Detailed explanation here.

```rust
fn hello_world() {
    println!("Hello, Rust RAG!");
}
```

## Section 2: Conclusion
Final thoughts.
"#;

        let parsed = MarkdownParser::parse(md);

        assert_eq!(parsed.title.as_deref(), Some("Main Document Title"));
        assert_eq!(parsed.sections.len(), 4);

        // Section 0: H1
        assert_eq!(parsed.sections[0].level, 1);
        assert_eq!(parsed.sections[0].title, "Main Document Title");
        assert_eq!(parsed.sections[0].breadcrumbs, vec!["Main Document Title"]);
        assert!(
            parsed.sections[0]
                .content
                .contains("This is an introduction paragraph.\n\nSecond intro paragraph.")
        );

        // Section 1: H2
        assert_eq!(parsed.sections[1].level, 2);
        assert_eq!(parsed.sections[1].title, "Section 1: Overview");
        assert_eq!(
            parsed.sections[1].breadcrumbs,
            vec!["Main Document Title", "Section 1: Overview"]
        );
        assert!(parsed.sections[1].content.contains("`inline_code`"));
        assert!(parsed.sections[1].content.contains("- Item A"));
        assert!(parsed.sections[1].content.contains("- Item B"));

        // Section 2: H3 with code block
        assert_eq!(parsed.sections[2].level, 3);
        assert_eq!(parsed.sections[2].title, "Subsection 1.1: Deep Dive");
        assert_eq!(
            parsed.sections[2].breadcrumbs,
            vec![
                "Main Document Title",
                "Section 1: Overview",
                "Subsection 1.1: Deep Dive"
            ]
        );
        assert_eq!(parsed.sections[2].code_blocks.len(), 1);
        assert_eq!(
            parsed.sections[2].code_blocks[0].language.as_deref(),
            Some("rust")
        );
        assert!(
            parsed.sections[2].code_blocks[0]
                .code
                .contains("fn hello_world()")
        );

        // Section 3: H2 popping H3
        assert_eq!(parsed.sections[3].level, 2);
        assert_eq!(parsed.sections[3].title, "Section 2: Conclusion");
        assert_eq!(
            parsed.sections[3].breadcrumbs,
            vec!["Main Document Title", "Section 2: Conclusion"]
        );
        assert!(parsed.sections[3].content.contains("Final thoughts."));
    }

    #[test]
    fn test_parse_preamble_without_h1() {
        let md = "Introductory text before any heading.\n\n## First Heading\nContent under first heading.";
        let parsed = MarkdownParser::parse(md);
        assert_eq!(parsed.title, None);
        assert_eq!(parsed.sections.len(), 2);
        assert_eq!(parsed.sections[0].level, 0);
        assert_eq!(parsed.sections[0].title, "");
        assert!(parsed.sections[0].content.contains("Introductory text"));
        assert_eq!(parsed.sections[1].level, 2);
        assert_eq!(parsed.sections[1].title, "First Heading");
    }
}
