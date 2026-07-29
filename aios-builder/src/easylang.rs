use crate::workflow::{Workflow, WorkflowStep};

#[derive(Debug)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

pub struct EasyLangParser;

impl EasyLangParser {
    pub fn parse(text: &str, workflow_name: &str) -> Result<Workflow, Vec<ParseError>> {
        let mut steps = Vec::new();
        let mut errors = Vec::new();

        for (i, raw_line) in text.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with("//") || line.starts_with('#') {
                continue;
            }

            match Self::parse_line(line, i + 1) {
                Ok(step) => steps.push(step),
                Err(e) => errors.push(e),
            }
        }

        if !errors.is_empty() {
            return Err(errors);
        }

        let mut wf = Workflow::new(workflow_name);
        for step in steps {
            wf.steps.push(step);
        }
        Ok(wf)
    }

    fn parse_line(line: &str, line_num: usize) -> Result<WorkflowStep, ParseError> {
        let line = line.trim();

        let (label_prefix, rest) = if let Some(pos) = line.find(':') {
            let before = line[..pos].trim();
            let after = line[pos + 1..].trim();
            let label = if before.contains(' ') {
                return Err(ParseError {
                    line: line_num,
                    message: format!("Label cannot contain spaces: \"{before}\""),
                });
            } else if !before.is_empty() {
                Some(before.to_string())
            } else {
                None
            };
            (label, after)
        } else {
            (None, line)
        };

        let prompt = rest.to_string();
        let label = label_prefix.unwrap_or_else(|| Self::auto_label(rest));

        Ok(WorkflowStep { label, prompt })
    }

    fn auto_label(line: &str) -> String {
        let mut label = String::new();
        for ch in line.chars() {
            if ch.is_alphanumeric() || ch == '_' || ch == '-' {
                label.push(ch);
            } else if ch.is_whitespace() && !label.is_empty() && !label.ends_with('_') {
                label.push('_');
            }
        }
        if label.ends_with('_') {
            label.truncate(label.len() - 1);
        }
        if label.is_empty() {
            label = "step".to_string();
        }
        label
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty() {
        let wf = EasyLangParser::parse("", "empty").unwrap();
        assert_eq!(wf.steps.len(), 0);
    }

    #[test]
    fn test_parse_comments_only() {
        let text = "// this is a comment\n# another comment\n  // indented";
        let wf = EasyLangParser::parse(text, "comments").unwrap();
        assert_eq!(wf.steps.len(), 0);
    }

    #[test]
    fn test_parse_single_spawn() {
        let wf = EasyLangParser::parse(r#"spawn "firefox""#, "wf").unwrap();
        assert_eq!(wf.steps.len(), 1);
        assert_eq!(wf.steps[0].prompt, r#"spawn "firefox""#);
        assert_eq!(wf.steps[0].label, "spawn_firefox");
    }

    #[test]
    fn test_parse_with_label_prefix() {
        let wf = EasyLangParser::parse("launch: spawn \"chrome\"", "wf").unwrap();
        assert_eq!(wf.steps.len(), 1);
        assert_eq!(wf.steps[0].label, "launch");
        assert_eq!(wf.steps[0].prompt, "spawn \"chrome\"");
    }

    #[test]
    fn test_parse_multiple_commands() {
        let text = r#"
            spawn "browser"
            timer 5000
            load "network"
            unload "network"
            kill "process"
            query "memory"
            compact
            status
        "#;
        let wf = EasyLangParser::parse(text, "multi").unwrap();
        assert_eq!(wf.steps.len(), 8);
        assert_eq!(wf.steps[0].label, "spawn_browser");
        assert_eq!(wf.steps[1].label, "timer_5000");
        assert_eq!(wf.steps[2].label, "load_network");
        assert_eq!(wf.steps[3].label, "unload_network");
        assert_eq!(wf.steps[4].label, "kill_process");
        assert_eq!(wf.steps[5].label, "query_memory");
        assert_eq!(wf.steps[6].label, "compact");
        assert_eq!(wf.steps[7].label, "status");
    }

    #[test]
    fn test_parse_custom_labels() {
        let text = "step1: spawn \"x\"\nstep2: spawn \"y\"\nstep3: spawn \"z\"";
        let wf = EasyLangParser::parse(text, "custom").unwrap();
        assert_eq!(wf.steps.len(), 3);
        assert_eq!(wf.steps[0].label, "step1");
        assert_eq!(wf.steps[1].label, "step2");
        assert_eq!(wf.steps[2].label, "step3");
    }

    #[test]
    fn test_parse_label_with_spaces_error() {
        let result = EasyLangParser::parse("my label: spawn \"x\"", "err");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_workflow_name() {
        let wf = EasyLangParser::parse("spawn \"x\"", "my_workflow").unwrap();
        assert_eq!(wf.name, "my_workflow");
    }

    #[test]
    fn test_parse_non_ascii_label() {
        let wf = EasyLangParser::parse("привет: spawn \"x\"", "unicode").unwrap();
        assert_eq!(wf.steps[0].label, "привет");
    }

    #[test]
    fn test_parse_to_json_roundtrip() {
        let wf1 = EasyLangParser::parse("spawn \"test\"\ncompact", "rt").unwrap();
        let json = wf1.to_json().unwrap();
        let wf2 = Workflow::from_json(&json).unwrap();
        assert_eq!(wf1.steps.len(), wf2.steps.len());
        assert_eq!(wf1.steps[0].prompt, wf2.steps[0].prompt);
        assert_eq!(wf1.steps[1].label, wf2.steps[1].label);
    }
}
