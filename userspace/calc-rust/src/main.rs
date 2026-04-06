use std::io::{self, BufRead, Write};

/// Recursive descent parser for arithmetic expressions.
///
/// Grammar:
///   expr   = term (('+' | '-') term)*
///   term   = factor (('*' | '/') factor)*
///   factor = '-' factor | '(' expr ')' | number

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            pos: 0,
        }
    }

    fn skip_whitespace(&mut self) {
        while self.pos < self.input.len() && self.input[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_whitespace();
        self.input.get(self.pos).copied()
    }

    fn consume(&mut self) -> u8 {
        let ch = self.input[self.pos];
        self.pos += 1;
        ch
    }

    fn parse_number(&mut self) -> Result<f64, String> {
        self.skip_whitespace();
        let start = self.pos;
        while self.pos < self.input.len()
            && (self.input[self.pos].is_ascii_digit() || self.input[self.pos] == b'.')
        {
            self.pos += 1;
        }
        if self.pos == start {
            return Err("expected number".to_string());
        }
        let s = std::str::from_utf8(&self.input[start..self.pos]).unwrap();
        s.parse::<f64>().map_err(|e| e.to_string())
    }

    fn parse_factor(&mut self) -> Result<f64, String> {
        match self.peek() {
            Some(b'-') => {
                self.consume();
                let v = self.parse_factor()?;
                Ok(-v)
            }
            Some(b'(') => {
                self.consume();
                let v = self.parse_expr()?;
                if self.peek() != Some(b')') {
                    return Err("expected ')'".to_string());
                }
                self.consume();
                Ok(v)
            }
            _ => self.parse_number(),
        }
    }

    fn parse_term(&mut self) -> Result<f64, String> {
        let mut left = self.parse_factor()?;
        loop {
            match self.peek() {
                Some(b'*') => {
                    self.consume();
                    left *= self.parse_factor()?;
                }
                Some(b'/') => {
                    self.consume();
                    let right = self.parse_factor()?;
                    if right == 0.0 {
                        return Err("division by zero".to_string());
                    }
                    left /= right;
                }
                _ => break,
            }
        }
        Ok(left)
    }

    fn parse_expr(&mut self) -> Result<f64, String> {
        let mut left = self.parse_term()?;
        loop {
            match self.peek() {
                Some(b'+') => {
                    self.consume();
                    left += self.parse_term()?;
                }
                Some(b'-') => {
                    self.consume();
                    left -= self.parse_term()?;
                }
                _ => break,
            }
        }
        Ok(left)
    }

    fn parse(mut self) -> Result<f64, String> {
        let result = self.parse_expr()?;
        self.skip_whitespace();
        if self.pos != self.input.len() {
            return Err(format!("unexpected character at position {}", self.pos));
        }
        Ok(result)
    }
}

fn main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    let _ = write!(stdout, "> ");
    let _ = stdout.flush();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            let _ = write!(stdout, "> ");
            let _ = stdout.flush();
            continue;
        }
        if trimmed == "quit" {
            break;
        }
        match Parser::new(trimmed).parse() {
            Ok(value) => {
                if value == value.floor() && value.abs() < 1e15 {
                    let _ = writeln!(stdout, "{}", value as i64);
                } else {
                    let _ = writeln!(stdout, "{value}");
                }
            }
            Err(e) => {
                let _ = writeln!(stdout, "error: {e}");
            }
        }
        let _ = write!(stdout, "> ");
        let _ = stdout.flush();
    }
}
