//! The command's exit codes: the one table the command exits by, `corduroy codes` prints and
//! the reference lists.
//!
//! A code is a contract with whatever program starts the command: once given, a number keeps
//! its meaning, and a new ending takes a new number.

use std::error::Error;
use std::process::ExitCode;

use corduroy_harness::Status;

use crate::terminal::{Out, say};

/// One way the command ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Code {
    /// What the process exits with.
    pub number: u8,
    /// The ending in a word or two, as a `turn/end` event's `status` has it where the
    /// ending is a run's.
    pub name: &'static str,
    /// What happened, and what whoever started the command can do next.
    pub meaning: &'static str,
}

const OK: Code = Code {
    number: 0,
    name: "finished",
    meaning: "The run ended with the model's answer (or with nothing left to do), or the \
              command did what it was asked.",
};
const FAILED: Code = Code {
    number: 1,
    name: "failed",
    meaning: "The run failed: the model could not be asked or gave no answer, or the run was \
              picked up too often. Also any error of the command itself, which it says on \
              standard error after `corduroy: `, and `check` finding a plugin that did not \
              start. The reason is in the last line printed, and in the run's `turn/end`.",
};
const TURN_LIMIT: Code = Code {
    number: 3,
    name: "turn-limit",
    meaning: "The model was still working when the run reached `max_turns`. What it did is \
              done and in the log; another `run` goes on in the same conversation.",
};
const TIME_LIMIT: Code = Code {
    number: 4,
    name: "time-limit",
    meaning: "The model was still working when the run reached `max_seconds`. What it did is \
              done and in the log; another `run` goes on in the same conversation.",
};
const OUTPUT_LIMIT: Code = Code {
    number: 5,
    name: "output-limit",
    meaning: "The model was still working when it had written `max_output_tokens` in the \
              run. What it did is done and in the log; another `run` goes on in the same \
              conversation.",
};
const CUT_OFF: Code = Code {
    number: 6,
    name: "cut-off",
    meaning: "The model answered, and the answer stopped at the most it may write in one \
              answer (`max_tokens`), so it is not whole. `run go on` asks for the rest.",
};
const WAITING: Code = Code {
    number: 7,
    name: "waiting",
    meaning: "The run is not over: it waits, in the log, for a person's answer to a gate's \
              question, or for a signal. `continue` takes it on and reads the answers \
              (`y`, or anything else as the reason for no) from standard input; `stop` ends \
              it.",
};
const STOPPED: Code = Code {
    number: 130,
    name: "stopped",
    meaning: "The run was stopped, by Ctrl-C (SIGINT) or `stop`, as a program interrupted \
              from the keyboard exits.",
};

/// Every code the command exits with, in order. Code 2 is kept free: it is what shells and
/// many commands give a mistake in how they were called.
pub const CODES: [Code; 8] = [
    OK,
    FAILED,
    TURN_LIMIT,
    TIME_LIMIT,
    OUTPUT_LIMIT,
    CUT_OFF,
    WAITING,
    STOPPED,
];

/// The code a command that ran a run to `status` ends with.
pub(super) const fn of(status: &Status) -> Code {
    match status {
        Status::Finished { .. } | Status::Yielded => OK,
        Status::CutOff { .. } => CUT_OFF,
        Status::TurnLimitReached => TURN_LIMIT,
        Status::TimeLimitReached => TIME_LIMIT,
        Status::OutputLimitReached => OUTPUT_LIMIT,
        Status::Waiting { .. } => WAITING,
        Status::Stopped => STOPPED,
        // A way of ending this command does not know yet is no success.
        _ => FAILED,
    }
}

impl From<Code> for ExitCode {
    fn from(code: Code) -> Self {
        Self::from(code.number)
    }
}

/// One code as `corduroy codes` prints it.
fn line(code: &Code) -> String {
    format!("{:<3} {:<12} {}", code.number, code.name, code.meaning)
}

/// Print what each exit code means, or what the one in `asked` does.
pub(super) fn codes(asked: Option<&str>, out: &Out) -> Result<ExitCode, Box<dyn Error>> {
    let Some(asked) = asked else {
        for code in &CODES {
            say(out, &line(code));
        }
        return Ok(ExitCode::SUCCESS);
    };
    let known = || {
        let numbers: Vec<String> = CODES.iter().map(|code| code.number.to_string()).collect();
        numbers.join(", ")
    };
    let code = asked
        .parse::<u8>()
        .ok()
        .and_then(|number| CODES.iter().find(|code| code.number == number))
        .ok_or_else(|| {
            format!(
                "`{asked}` is not a code this command exits with: those are {}",
                known()
            )
        })?;
    say(out, &line(code));
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_ending_has_a_number_of_its_own_outside_what_shells_use() {
        for (index, code) in CODES.iter().enumerate() {
            let twice = CODES
                .iter()
                .skip(index + 1)
                .any(|other| other.number == code.number || other.name == code.name);
            assert!(!twice, "{code:?}");
            // 2 is a mistake in the call; 126 to 165 are the shell's and signals'.
            let free = code.number != 2 && !(126..=165).contains(&code.number);
            assert!(free || *code == STOPPED, "{code:?}");
        }
    }

    #[test]
    fn a_run_exits_by_how_it_ended() {
        let finished = Status::Finished {
            answer: String::new(),
        };
        let failed = Status::Failed {
            reason: String::new(),
        };
        let waiting = Status::Waiting {
            asks: Vec::new(),
            waits: Vec::new(),
        };
        let cut = Status::CutOff {
            answer: String::new(),
        };
        let numbers = [
            &finished,
            &Status::Yielded,
            &failed,
            &Status::TurnLimitReached,
            &Status::TimeLimitReached,
            &Status::OutputLimitReached,
            &cut,
            &waiting,
            &Status::Stopped,
        ]
        .map(|status| of(status).number);
        assert_eq!(numbers, [0, 0, 1, 3, 4, 5, 6, 7, 130]);
    }
}
