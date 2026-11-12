//! Questions for the user: a permission prompt for a tool, or a question
//! the model or an engine extension asks, with or without options.
//!
//! Lejce only carries them. The program shows the question in its own way
//! and returns the answer.

/// A question for the user.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Question {
    /// The tool that waits for permission. None for a question the model
    /// or an engine extension asks.
    pub tool: Option<String>,
    /// The question itself, short.
    pub text: String,
    /// More about it: the call a tool would make, for example.
    pub detail: String,
    /// The options to choose from. Empty for a yes-or-no question.
    pub options: Vec<String>,
    /// The user may choose several options.
    pub multiple: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Yes,
    /// No, or the user cancelled the choice.
    No,
    /// The chosen options, by their index in `Question::options`.
    Chose(Vec<usize>),
    /// Nobody answered: the time ran out, or the program could not ask.
    Unanswered,
}
