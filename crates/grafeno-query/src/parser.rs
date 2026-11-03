//! A recursive-descent parser from tokens to the syntax tree.

use grafeno::Direction;

use crate::QueryError;
use crate::ast::{
    BinOp, Clause, Expr, Key, NodePattern, Pattern, Query, RelPattern, RemoveItem, Return,
    ReturnItem, SetItem, UnOp,
};
use crate::lexer::{Spanned, Token, lex};
use crate::value::Value;

pub(crate) fn parse(source: &str) -> Result<Query, QueryError> {
    let mut parser = Parser {
        source,
        tokens: lex(source)?,
        position: 0,
    };
    let query = parser.query()?;
    parser.eat(&Token::Semicolon);
    if parser.peek() != &Token::End {
        return Err(parser.unexpected("the end of the query (one statement at a time)"));
    }
    Ok(query)
}

struct Parser<'a> {
    source: &'a str,
    tokens: Vec<Spanned>,
    position: usize,
}

impl Parser<'_> {
    fn peek(&self) -> &Token {
        &self.tokens[self.position].token
    }

    fn peek_at(&self, ahead: usize) -> &Token {
        let last = self.tokens.len() - 1;
        &self.tokens[(self.position + ahead).min(last)].token
    }

    fn start(&self) -> usize {
        self.tokens[self.position].start
    }

    fn previous_end(&self) -> usize {
        self.position
            .checked_sub(1)
            .map_or(0, |index| self.tokens[index].end)
    }

    fn advance(&mut self) -> Token {
        let token = self.tokens[self.position].token.clone();
        if token != Token::End {
            self.position += 1;
        }
        token
    }

    fn eat(&mut self, token: &Token) -> bool {
        if self.peek() == token {
            self.advance();
            true
        } else {
            false
        }
    }

    fn error(&self, message: impl Into<String>) -> QueryError {
        QueryError::at(self.source, self.start(), message)
    }

    fn unexpected(&self, expected: &str) -> QueryError {
        self.error(format!(
            "expected {expected}, found {}",
            self.peek().describe()
        ))
    }

    fn expect(&mut self, token: Token, expected: &str) -> Result<(), QueryError> {
        if self.eat(&token) {
            Ok(())
        } else {
            Err(self.unexpected(expected))
        }
    }

    /// Whether the next token is the keyword `word` (any case).
    fn at_keyword(&self, word: &str) -> bool {
        matches!(self.peek(), Token::Ident(name) if name.eq_ignore_ascii_case(word))
    }

    fn keyword_at(&self, ahead: usize, word: &str) -> bool {
        matches!(self.peek_at(ahead), Token::Ident(name) if name.eq_ignore_ascii_case(word))
    }

    fn eat_keyword(&mut self, word: &str) -> bool {
        if self.at_keyword(word) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_keyword(&mut self, word: &str) -> Result<(), QueryError> {
        if self.eat_keyword(word) {
            Ok(())
        } else {
            Err(self.unexpected(&format!("`{word}`")))
        }
    }

    fn ident(&mut self, expected: &str) -> Result<String, QueryError> {
        match self.peek().clone() {
            Token::Ident(name) => {
                self.advance();
                Ok(name)
            }
            _ => Err(self.unexpected(expected)),
        }
    }

    // ------------------------------------------------------------ clauses

    fn query(&mut self) -> Result<Query, QueryError> {
        let mut clauses = Vec::new();
        loop {
            match self.peek() {
                Token::End | Token::Semicolon => break,
                _ => clauses.push(self.clause()?),
            }
        }
        if clauses.is_empty() {
            return Err(self.error("the query is empty"));
        }
        Ok(Query { clauses })
    }

    fn clause(&mut self) -> Result<Clause, QueryError> {
        if self.eat_keyword("MATCH") {
            let mut patterns = vec![self.pattern()?];
            while self.eat(&Token::Comma) {
                patterns.push(self.pattern()?);
            }
            let (mut valid_at, mut known_at) = (None, None);
            loop {
                if self.at_keyword("VALID") && self.keyword_at(1, "AT") {
                    self.position += 2;
                    valid_at = Some(self.expr()?);
                } else if self.at_keyword("KNOWN") && self.keyword_at(1, "AT") {
                    self.position += 2;
                    known_at = Some(self.expr()?);
                } else {
                    break;
                }
            }
            let filter = if self.eat_keyword("WHERE") {
                Some(self.expr()?)
            } else {
                None
            };
            return Ok(Clause::Match {
                patterns,
                valid_at,
                known_at,
                filter,
            });
        }
        if self.eat_keyword("CREATE") {
            let mut patterns = vec![self.pattern()?];
            while self.eat(&Token::Comma) {
                patterns.push(self.pattern()?);
            }
            return Ok(Clause::Create(patterns));
        }
        if self.eat_keyword("SET") {
            let mut items = vec![self.set_item()?];
            while self.eat(&Token::Comma) {
                items.push(self.set_item()?);
            }
            return Ok(Clause::Set(items));
        }
        if self.eat_keyword("REMOVE") {
            let mut items = vec![self.remove_item()?];
            while self.eat(&Token::Comma) {
                items.push(self.remove_item()?);
            }
            return Ok(Clause::Remove(items));
        }
        if self.at_keyword("DETACH") && self.keyword_at(1, "DELETE") {
            self.position += 1;
        }
        if self.eat_keyword("DELETE") {
            return Ok(Clause::Delete(self.expr_list()?));
        }
        if self.eat_keyword("RETRACT") {
            return Ok(Clause::Retract(self.expr_list()?));
        }
        if self.eat_keyword("RETURN") {
            return Ok(Clause::Return(self.return_clause()?));
        }
        for unsupported in [
            "OPTIONAL", "MERGE", "WITH", "UNWIND", "CALL", "UNION", "FOREACH",
        ] {
            if self.at_keyword(unsupported) {
                return Err(self.error(format!("`{unsupported}` is not supported yet")));
            }
        }
        if self.at_keyword("WHERE") {
            return Err(self.error("`WHERE` must follow a `MATCH`"));
        }
        Err(self.unexpected("a clause (MATCH, CREATE, SET, REMOVE, DELETE, RETRACT or RETURN)"))
    }

    fn expr_list(&mut self) -> Result<Vec<Expr>, QueryError> {
        let mut items = vec![self.expr()?];
        while self.eat(&Token::Comma) {
            items.push(self.expr()?);
        }
        Ok(items)
    }

    fn return_clause(&mut self) -> Result<Return, QueryError> {
        let distinct = self.eat_keyword("DISTINCT");
        let items = if self.eat(&Token::Star) {
            None
        } else {
            let mut items = vec![self.return_item()?];
            while self.eat(&Token::Comma) {
                items.push(self.return_item()?);
            }
            Some(items)
        };
        let mut order = Vec::new();
        if self.at_keyword("ORDER") {
            self.advance();
            self.expect_keyword("BY")?;
            loop {
                let expr = self.expr()?;
                let descending = if self.eat_keyword("DESC") || self.eat_keyword("DESCENDING") {
                    true
                } else {
                    let _ = self.eat_keyword("ASC") || self.eat_keyword("ASCENDING");
                    false
                };
                order.push((expr, descending));
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
        }
        let skip = if self.eat_keyword("SKIP") {
            Some(self.expr()?)
        } else {
            None
        };
        let limit = if self.eat_keyword("LIMIT") {
            Some(self.expr()?)
        } else {
            None
        };
        Ok(Return {
            distinct,
            items,
            order,
            skip,
            limit,
        })
    }

    fn return_item(&mut self) -> Result<ReturnItem, QueryError> {
        let start = self.start();
        let expr = self.expr()?;
        let text = self.source[start..self.previous_end()].trim().to_owned();
        let alias = if self.eat_keyword("AS") {
            self.ident("a column name after `AS`")?
        } else {
            text
        };
        Ok(ReturnItem { expr, alias })
    }

    fn set_item(&mut self) -> Result<SetItem, QueryError> {
        let var = self.ident("a variable")?;
        if self.eat(&Token::Dot) {
            let key = self.key()?;
            self.expect(Token::Eq, "`=`")?;
            return Ok(SetItem::Property {
                var,
                key,
                value: self.expr()?,
            });
        }
        if self.eat(&Token::PlusEq) {
            return Ok(SetItem::Merge {
                var,
                map: self.expr()?,
            });
        }
        if self.eat(&Token::Eq) {
            return Ok(SetItem::Replace {
                var,
                map: self.expr()?,
            });
        }
        if self.peek() == &Token::Colon {
            return Ok(SetItem::Labels {
                var,
                labels: self.labels()?,
            });
        }
        Err(self.unexpected("`.property =`, `+=`, `=` or `:Label`"))
    }

    fn remove_item(&mut self) -> Result<RemoveItem, QueryError> {
        let var = self.ident("a variable")?;
        if self.eat(&Token::Dot) {
            return Ok(RemoveItem::Property {
                var,
                key: self.key()?,
            });
        }
        if self.peek() == &Token::Colon {
            return Ok(RemoveItem::Labels {
                var,
                labels: self.labels()?,
            });
        }
        Err(self.unexpected("`.property` or `:Label`"))
    }

    fn key(&mut self) -> Result<Key, QueryError> {
        let key = match self.peek() {
            Token::Ident(name) => Key::Prop(name.clone()),
            Token::Meta(name) => Key::Meta(name.clone()),
            _ => return Err(self.unexpected("a property name")),
        };
        self.advance();
        Ok(key)
    }

    /// `:A:B`
    fn labels(&mut self) -> Result<Vec<String>, QueryError> {
        let mut labels = Vec::new();
        while self.eat(&Token::Colon) {
            labels.push(self.ident("a label")?);
        }
        Ok(labels)
    }

    // ------------------------------------------------------------ patterns

    fn pattern(&mut self) -> Result<Pattern, QueryError> {
        let start = self.node_pattern()?;
        let mut steps = Vec::new();
        while matches!(self.peek(), Token::Minus | Token::Lt) {
            let rel = self.rel_pattern()?;
            steps.push((rel, self.node_pattern()?));
        }
        Ok(Pattern { start, steps })
    }

    fn node_pattern(&mut self) -> Result<NodePattern, QueryError> {
        self.expect(Token::LParen, "`(` to start a node pattern")?;
        let var = match self.peek().clone() {
            Token::Ident(name) => {
                self.advance();
                Some(name)
            }
            _ => None,
        };
        let labels = self.labels()?;
        let props = if self.peek() == &Token::LBrace {
            self.props()?
        } else {
            Vec::new()
        };
        self.expect(Token::RParen, "`)` to close the node pattern")?;
        Ok(NodePattern { var, labels, props })
    }

    fn rel_pattern(&mut self) -> Result<RelPattern, QueryError> {
        let left = self.eat(&Token::Lt);
        self.expect(Token::Minus, "`-`")?;
        let mut rel = RelPattern {
            var: None,
            labels: Vec::new(),
            props: Vec::new(),
            direction: Direction::Both,
            length: None,
        };
        if self.eat(&Token::LBracket) {
            if let Token::Ident(name) = self.peek().clone() {
                self.advance();
                rel.var = Some(name);
            }
            if self.eat(&Token::Colon) {
                rel.labels.push(self.ident("an edge label")?);
                while self.eat(&Token::Pipe) {
                    self.eat(&Token::Colon);
                    rel.labels.push(self.ident("an edge label")?);
                }
            }
            if self.eat(&Token::Star) {
                rel.length = Some(self.range()?);
            }
            if self.peek() == &Token::LBrace {
                rel.props = self.props()?;
            }
            self.expect(Token::RBracket, "`]` to close the edge pattern")?;
        }
        self.expect(Token::Minus, "`-`")?;
        let right = self.eat(&Token::Gt);
        rel.direction = match (left, right) {
            (true, false) => Direction::In,
            (false, true) => Direction::Out,
            (false, false) => Direction::Both,
            (true, true) => return Err(self.error("an edge cannot point both ways (`<-…->`)")),
        };
        Ok(rel)
    }

    /// After `*`: ``, `n`, `n..`, `..m`, `n..m`.
    fn range(&mut self) -> Result<(u32, Option<u32>), QueryError> {
        let number = |parser: &mut Self| -> Result<Option<u32>, QueryError> {
            match parser.peek().clone() {
                Token::Int(n) => {
                    parser.advance();
                    u32::try_from(n)
                        .map(Some)
                        .map_err(|_| parser.error("path length out of range"))
                }
                _ => Ok(None),
            }
        };
        let low = number(self)?;
        if self.eat(&Token::DotDot) {
            let high = number(self)?;
            let low = low.unwrap_or(1);
            if high.is_some_and(|high| high < low) {
                return Err(self.error("the path length range is empty"));
            }
            return Ok((low, high));
        }
        Ok(match low {
            Some(n) => (n, Some(n)),
            None => (1, None),
        })
    }

    fn props(&mut self) -> Result<Vec<(Key, Expr)>, QueryError> {
        self.expect(Token::LBrace, "`{`")?;
        let mut props = Vec::new();
        if !self.eat(&Token::RBrace) {
            loop {
                let key = match self.peek() {
                    Token::Ident(name) | Token::Str(name) => Key::Prop(name.clone()),
                    Token::Meta(name) => Key::Meta(name.clone()),
                    _ => return Err(self.unexpected("a property name")),
                };
                self.advance();
                self.expect(Token::Colon, "`:` after the property name")?;
                props.push((key, self.expr()?));
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
            self.expect(Token::RBrace, "`}`")?;
        }
        Ok(props)
    }

    // ------------------------------------------------------------ expressions

    pub(crate) fn expr(&mut self) -> Result<Expr, QueryError> {
        self.or()
    }

    fn or(&mut self) -> Result<Expr, QueryError> {
        let mut left = self.xor()?;
        while self.eat_keyword("OR") {
            left = Expr::Binary(BinOp::Or, Box::new(left), Box::new(self.xor()?));
        }
        Ok(left)
    }

    fn xor(&mut self) -> Result<Expr, QueryError> {
        let mut left = self.and()?;
        while self.eat_keyword("XOR") {
            left = Expr::Binary(BinOp::Xor, Box::new(left), Box::new(self.and()?));
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Expr, QueryError> {
        let mut left = self.not()?;
        while self.eat_keyword("AND") {
            left = Expr::Binary(BinOp::And, Box::new(left), Box::new(self.not()?));
        }
        Ok(left)
    }

    fn not(&mut self) -> Result<Expr, QueryError> {
        if self.eat_keyword("NOT") {
            return Ok(Expr::Unary(UnOp::Not, Box::new(self.not()?)));
        }
        self.comparison()
    }

    fn comparison(&mut self) -> Result<Expr, QueryError> {
        let mut left = self.additive()?;
        loop {
            let op = match self.peek() {
                Token::Eq => Some(BinOp::Eq),
                Token::Neq => Some(BinOp::Neq),
                Token::Lt => Some(BinOp::Lt),
                Token::Le => Some(BinOp::Le),
                Token::Gt => Some(BinOp::Gt),
                Token::Ge => Some(BinOp::Ge),
                _ => None,
            };
            if let Some(op) = op {
                self.advance();
                left = Expr::Binary(op, Box::new(left), Box::new(self.additive()?));
            } else if self.eat_keyword("IS") {
                let negated = self.eat_keyword("NOT");
                self.expect_keyword("NULL")?;
                left = Expr::IsNull {
                    expr: Box::new(left),
                    negated,
                };
            } else if self.eat_keyword("CONTAINS") {
                left = Expr::Binary(BinOp::Contains, Box::new(left), Box::new(self.additive()?));
            } else if self.at_keyword("STARTS") && self.keyword_at(1, "WITH") {
                self.position += 2;
                left = Expr::Binary(
                    BinOp::StartsWith,
                    Box::new(left),
                    Box::new(self.additive()?),
                );
            } else if self.at_keyword("ENDS") && self.keyword_at(1, "WITH") {
                self.position += 2;
                left = Expr::Binary(BinOp::EndsWith, Box::new(left), Box::new(self.additive()?));
            } else if self.eat_keyword("IN") {
                left = Expr::Binary(BinOp::In, Box::new(left), Box::new(self.additive()?));
            } else if self.peek() == &Token::Colon {
                left = Expr::HasLabels(Box::new(left), self.labels()?);
            } else {
                return Ok(left);
            }
        }
    }

    fn additive(&mut self) -> Result<Expr, QueryError> {
        let mut left = self.multiplicative()?;
        loop {
            let op = match self.peek() {
                Token::Plus => BinOp::Add,
                Token::Minus => BinOp::Sub,
                _ => return Ok(left),
            };
            self.advance();
            left = Expr::Binary(op, Box::new(left), Box::new(self.multiplicative()?));
        }
    }

    fn multiplicative(&mut self) -> Result<Expr, QueryError> {
        let mut left = self.unary()?;
        loop {
            let op = match self.peek() {
                Token::Star => BinOp::Mul,
                Token::Slash => BinOp::Div,
                Token::Percent => BinOp::Mod,
                _ => return Ok(left),
            };
            self.advance();
            left = Expr::Binary(op, Box::new(left), Box::new(self.unary()?));
        }
    }

    fn unary(&mut self) -> Result<Expr, QueryError> {
        if self.eat(&Token::Minus) {
            return Ok(Expr::Unary(UnOp::Neg, Box::new(self.unary()?)));
        }
        if self.eat(&Token::Plus) {
            return self.unary();
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr, QueryError> {
        let mut expr = self.atom()?;
        loop {
            if self.eat(&Token::Dot) {
                expr = Expr::Property(Box::new(expr), self.key()?);
            } else if self.eat(&Token::LBracket) {
                let index = self.expr()?;
                self.expect(Token::RBracket, "`]`")?;
                expr = Expr::Index(Box::new(expr), Box::new(index));
            } else {
                return Ok(expr);
            }
        }
    }

    fn atom(&mut self) -> Result<Expr, QueryError> {
        match self.peek().clone() {
            Token::Int(n) => {
                self.advance();
                Ok(Expr::Literal(Value::Int(n)))
            }
            Token::Float(f) => {
                self.advance();
                Ok(Expr::Literal(Value::Float(f)))
            }
            Token::Str(s) => {
                self.advance();
                Ok(Expr::Literal(Value::Str(s)))
            }
            Token::Param(name) => {
                self.advance();
                Ok(Expr::Param(name))
            }
            Token::LParen => {
                self.advance();
                let expr = self.expr()?;
                self.expect(Token::RParen, "`)`")?;
                Ok(expr)
            }
            Token::LBracket => {
                self.advance();
                let mut items = Vec::new();
                if !self.eat(&Token::RBracket) {
                    items = self.expr_list()?;
                    self.expect(Token::RBracket, "`]` to close the list")?;
                }
                Ok(Expr::List(items))
            }
            Token::LBrace => {
                self.advance();
                let mut entries = Vec::new();
                if !self.eat(&Token::RBrace) {
                    loop {
                        let key = match self.peek() {
                            Token::Ident(name) | Token::Str(name) => name.clone(),
                            _ => return Err(self.unexpected("a key")),
                        };
                        self.advance();
                        self.expect(Token::Colon, "`:` after the key")?;
                        entries.push((key, self.expr()?));
                        if !self.eat(&Token::Comma) {
                            break;
                        }
                    }
                    self.expect(Token::RBrace, "`}` to close the map")?;
                }
                Ok(Expr::Map(entries))
            }
            Token::Ident(name) => {
                self.advance();
                match name.to_ascii_lowercase().as_str() {
                    "true" => return Ok(Expr::Literal(Value::Bool(true))),
                    "false" => return Ok(Expr::Literal(Value::Bool(false))),
                    "null" => return Ok(Expr::Literal(Value::Null)),
                    _ => {}
                }
                if !self.eat(&Token::LParen) {
                    return Ok(Expr::Var(name));
                }
                let name = name.to_ascii_lowercase();
                if name == "count" && self.peek() == &Token::Star {
                    self.advance();
                    self.expect(Token::RParen, "`)`")?;
                    return Ok(Expr::CountStar);
                }
                let distinct = self.eat_keyword("DISTINCT");
                let mut args = Vec::new();
                if !self.eat(&Token::RParen) {
                    args = self.expr_list()?;
                    self.expect(Token::RParen, "`)` to close the arguments")?;
                }
                Ok(Expr::Call {
                    name,
                    distinct,
                    args,
                })
            }
            _ => Err(self.unexpected("an expression")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_query() {
        let query = parse(
            "MATCH (a:Person {name: 'Ana'})-[r:works_at|knows*1..2]->(b), (c)
             VALID AT '2024-01-01' WHERE a.age >= 30 AND NOT b:Archived OR c.x IS NOT NULL
             RETURN DISTINCT a.name AS name, count(*), b.@id ORDER BY name DESC SKIP 1 LIMIT 5;",
        )
        .unwrap();
        assert_eq!(query.clauses.len(), 2);
        let Clause::Match {
            patterns,
            valid_at,
            filter,
            ..
        } = &query.clauses[0]
        else {
            panic!("expected MATCH");
        };
        assert_eq!(patterns.len(), 2);
        let (rel, target) = &patterns[0].steps[0];
        assert_eq!(rel.labels, ["works_at", "knows"]);
        assert_eq!(rel.direction, Direction::Out);
        assert_eq!(rel.length, Some((1, Some(2))));
        assert_eq!(target.var.as_deref(), Some("b"));
        assert!(valid_at.is_some() && filter.is_some());
        let Clause::Return(ret) = &query.clauses[1] else {
            panic!("expected RETURN");
        };
        let aliases: Vec<&str> = ret
            .items
            .as_ref()
            .unwrap()
            .iter()
            .map(|i| i.alias.as_str())
            .collect();
        assert_eq!(aliases, ["name", "count(*)", "b.@id"]);
        assert!(ret.distinct);
        assert!(ret.order[0].1);
    }

    #[test]
    fn parses_writes_and_directions() {
        let query = parse(
            "MATCH (a)<-[:x]-(b), (c)--(d) CREATE (a)-[:y {@valid_from: '2024'}]->(n:New {@text: 't'})
             SET a.p = 1, a += {q: 2}, a:L1:L2 REMOVE a.p, a:L1 DETACH DELETE b RETRACT r",
        )
        .unwrap();
        let Clause::Match { patterns, .. } = &query.clauses[0] else {
            panic!()
        };
        assert_eq!(patterns[0].steps[0].0.direction, Direction::In);
        assert_eq!(patterns[1].steps[0].0.direction, Direction::Both);
        assert_eq!(query.clauses.len(), 6);
        assert!(query.clauses[1..].iter().all(Clause::writes));
    }

    #[test]
    fn errors_point_at_the_problem() {
        let error = parse("MATCH (n:Person\nRETURN n").unwrap_err();
        assert_eq!(
            error.to_string(),
            "expected `)` to close the node pattern, found `RETURN` (line 2, column 1)"
        );
        assert!(
            parse("MATCH (a)<-[r]->(b) RETURN a")
                .unwrap_err()
                .to_string()
                .contains("both ways")
        );
        assert!(
            parse("MERGE (n) RETURN n")
                .unwrap_err()
                .to_string()
                .contains("not supported")
        );
        assert!(
            parse("RETURN 1; RETURN 2")
                .unwrap_err()
                .to_string()
                .contains("one statement")
        );
        assert!(parse("").unwrap_err().to_string().contains("empty"));
    }
}
