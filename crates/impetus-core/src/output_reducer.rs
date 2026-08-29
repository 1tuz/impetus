//! Output reducer: token-bounded summarization of tool outputs.
//!
//! Large tool outputs (test logs, diffs, search results) are reduced to fit
//! within token budgets while preserving key information for the model.

use std::borrow::Cow;

/// Token budget for output reduction
#[derive(Debug, Clone, Copy)]
pub struct TokenBudget {
    pub max_tokens: usize,
}

impl Default for TokenBudget {
    fn default() -> Self {
        Self { max_tokens: 2000 }
    }
}

/// Output reducer with token budget enforcement
pub struct OutputReducer {
    budget: TokenBudget,
}

impl OutputReducer {
    pub fn new(budget: TokenBudget) -> Self {
        Self { budget }
    }

    /// Reduce raw output to fit within token budget
    pub fn reduce<'a>(&self, raw_output: &'a str) -> ReducedOutput<'a> {
        let estimated_tokens = estimate_tokens(raw_output);

        if estimated_tokens <= self.budget.max_tokens {
            return ReducedOutput {
                content: Cow::Borrowed(raw_output),
                truncated: false,
                original_tokens: estimated_tokens,
                reduced_tokens: estimated_tokens,
            };
        }

        // Simple line-based truncation with head/tail preservation
        let lines: Vec<&str> = raw_output.lines().collect();
        let total_lines = lines.len();

        if total_lines <= 10 {
            // For very short outputs, just truncate characters
            let reduced = truncate_to_budget(raw_output, self.budget.max_tokens);
            return ReducedOutput {
                content: Cow::Owned(reduced.clone()),
                truncated: true,
                original_tokens: estimated_tokens,
                reduced_tokens: estimate_tokens(&reduced),
            };
        }

        // Keep first and last lines, omit middle
        let head_lines = total_lines / 3;
        let tail_lines = total_lines / 3;
        let omitted = total_lines - head_lines - tail_lines;

        let mut reduced = String::new();
        for line in lines.iter().take(head_lines) {
            reduced.push_str(line);
            reduced.push('\n');
        }

        reduced.push_str(&format!("\n... ({} lines omitted) ...\n\n", omitted));

        for line in lines.iter().skip(total_lines - tail_lines) {
            reduced.push_str(line);
            reduced.push('\n');
        }

        // If still too large, truncate further
        if estimate_tokens(&reduced) > self.budget.max_tokens {
            reduced = truncate_to_budget(&reduced, self.budget.max_tokens);
        }

        ReducedOutput {
            content: Cow::Owned(reduced.clone()),
            truncated: true,
            original_tokens: estimated_tokens,
            reduced_tokens: estimate_tokens(&reduced),
        }
    }

    /// Reduce with custom strategy
    pub fn reduce_with_strategy<'a>(
        &self,
        raw_output: &'a str,
        strategy: ReductionStrategy,
    ) -> ReducedOutput<'a> {
        match strategy {
            ReductionStrategy::HeadOnly => self.reduce_head_only(raw_output),
            ReductionStrategy::TailOnly => self.reduce_tail_only(raw_output),
            ReductionStrategy::HeadAndTail => self.reduce(raw_output),
            ReductionStrategy::ErrorsOnly => self.reduce_errors_only(raw_output),
        }
    }

    fn reduce_head_only<'a>(&self, raw_output: &'a str) -> ReducedOutput<'a> {
        let estimated_tokens = estimate_tokens(raw_output);
        if estimated_tokens <= self.budget.max_tokens {
            return ReducedOutput {
                content: Cow::Borrowed(raw_output),
                truncated: false,
                original_tokens: estimated_tokens,
                reduced_tokens: estimated_tokens,
            };
        }

        let reduced = truncate_to_budget(raw_output, self.budget.max_tokens);
        ReducedOutput {
            content: Cow::Owned(reduced.clone()),
            truncated: true,
            original_tokens: estimated_tokens,
            reduced_tokens: estimate_tokens(&reduced),
        }
    }

    fn reduce_tail_only<'a>(&self, raw_output: &'a str) -> ReducedOutput<'a> {
        let estimated_tokens = estimate_tokens(raw_output);
        if estimated_tokens <= self.budget.max_tokens {
            return ReducedOutput {
                content: Cow::Borrowed(raw_output),
                truncated: false,
                original_tokens: estimated_tokens,
                reduced_tokens: estimated_tokens,
            };
        }

        let lines: Vec<&str> = raw_output.lines().collect();
        let mut reduced = String::new();
        let mut current_tokens = 0;

        // Take lines from the end until budget exhausted
        for line in lines.iter().rev() {
            let line_tokens = estimate_tokens(line);
            if current_tokens + line_tokens > self.budget.max_tokens {
                break;
            }
            reduced.insert_str(0, line);
            reduced.insert(0, '\n');
            current_tokens += line_tokens;
        }

        ReducedOutput {
            content: Cow::Owned(reduced),
            truncated: true,
            original_tokens: estimated_tokens,
            reduced_tokens: current_tokens,
        }
    }

    fn reduce_errors_only<'a>(&self, raw_output: &'a str) -> ReducedOutput<'a> {
        let estimated_tokens = estimate_tokens(raw_output);
        let error_lines: Vec<&str> = raw_output
            .lines()
            .filter(|line| {
                let lower = line.to_lowercase();
                lower.contains("error")
                    || lower.contains("failed")
                    || lower.contains("panic")
                    || lower.contains("assertion")
            })
            .collect();

        if error_lines.is_empty() {
            return self.reduce_head_only(raw_output);
        }

        let reduced = error_lines.join("\n");
        let reduced_tokens = estimate_tokens(&reduced);

        if reduced_tokens > self.budget.max_tokens {
            let truncated = truncate_to_budget(&reduced, self.budget.max_tokens);
            return ReducedOutput {
                content: Cow::Owned(truncated.clone()),
                truncated: true,
                original_tokens: estimated_tokens,
                reduced_tokens: estimate_tokens(&truncated),
            };
        }

        ReducedOutput {
            content: Cow::Owned(reduced),
            truncated: error_lines.len() < raw_output.lines().count(),
            original_tokens: estimated_tokens,
            reduced_tokens,
        }
    }
}

impl Default for OutputReducer {
    fn default() -> Self {
        Self::new(TokenBudget::default())
    }
}

/// Reduced output with metadata
#[derive(Debug, Clone)]
pub struct ReducedOutput<'a> {
    pub content: Cow<'a, str>,
    pub truncated: bool,
    pub original_tokens: usize,
    pub reduced_tokens: usize,
}

/// Reduction strategy
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReductionStrategy {
    /// Keep only the beginning
    HeadOnly,
    /// Keep only the end
    TailOnly,
    /// Keep head and tail, omit middle
    HeadAndTail,
    /// Extract error/failure lines only
    ErrorsOnly,
}

/// Estimate token count (rough approximation: ~4 chars per token)
fn estimate_tokens(text: &str) -> usize {
    (text.len() + 3) / 4
}

/// Truncate text to approximate token budget
fn truncate_to_budget(text: &str, max_tokens: usize) -> String {
    let max_chars = max_tokens * 4;
    if text.len() <= max_chars {
        return text.to_string();
    }

    let mut truncated = text.chars().take(max_chars).collect::<String>();
    truncated.push_str("\n... (truncated)");
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_output_not_reduced() {
        let reducer = OutputReducer::new(TokenBudget { max_tokens: 1000 });
        let output = "Short output\nJust a few lines\n";
        let reduced = reducer.reduce(output);

        assert!(!reduced.truncated);
        assert_eq!(reduced.content, output);
    }

    #[test]
    fn large_output_truncated() {
        let reducer = OutputReducer::new(TokenBudget { max_tokens: 50 });
        let output = (0..100).map(|i| format!("Line {}", i)).collect::<Vec<_>>().join("\n");
        let reduced = reducer.reduce(&output);

        assert!(reduced.truncated);
        assert!(reduced.reduced_tokens < reduced.original_tokens);
    }

    #[test]
    fn head_and_tail_strategy() {
        let reducer = OutputReducer::new(TokenBudget { max_tokens: 100 });
        let lines: Vec<String> = (0..100).map(|i| format!("Line {}", i)).collect();
        let output = lines.join("\n");
        let reduced = reducer.reduce(&output);

        assert!(reduced.truncated);
        assert!(reduced.content.contains("Line 0"));
        assert!(reduced.content.contains("omitted"));
    }

    #[test]
    fn head_only_strategy() {
        let reducer = OutputReducer::new(TokenBudget { max_tokens: 50 });
        let output = (0..100).map(|i| format!("Line {}", i)).collect::<Vec<_>>().join("\n");
        let reduced = reducer.reduce_with_strategy(&output, ReductionStrategy::HeadOnly);

        assert!(reduced.truncated);
        assert!(reduced.content.contains("Line 0"));
    }

    #[test]
    fn tail_only_strategy() {
        let reducer = OutputReducer::new(TokenBudget { max_tokens: 50 });
        let output = (0..100).map(|i| format!("Line {}", i)).collect::<Vec<_>>().join("\n");
        let reduced = reducer.reduce_with_strategy(&output, ReductionStrategy::TailOnly);

        assert!(reduced.truncated);
        assert!(reduced.content.contains("Line 99"));
    }

    #[test]
    fn errors_only_strategy() {
        let reducer = OutputReducer::new(TokenBudget { max_tokens: 200 });
        let output = "\
Line 1: info message
Line 2: error: compilation failed
Line 3: debug output
Line 4: panic at src/main.rs:10
Line 5: more info
";
        let reduced = reducer.reduce_with_strategy(output, ReductionStrategy::ErrorsOnly);

        assert!(reduced.truncated);
        assert!(reduced.content.contains("error"));
        assert!(reduced.content.contains("panic"));
        assert!(!reduced.content.contains("info message"));
    }

    #[test]
    fn token_estimation() {
        assert_eq!(estimate_tokens("test"), 1);
        assert_eq!(estimate_tokens("a".repeat(100).as_str()), 25);
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn truncate_preserves_validity() {
        let text = "Hello world! This is a test.";
        let truncated = truncate_to_budget(text, 2);
        assert!(truncated.len() < text.len());
        assert!(truncated.contains("truncated"));
    }
}
