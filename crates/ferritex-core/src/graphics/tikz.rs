use crate::graphics::api::{
    Color, GraphicNode, GraphicText, GraphicsScene, PathSegment, Point, VectorPrimitive,
};

const CM_IN_PT: f64 = 28.3465;
const DEFAULT_LINE_WIDTH_PT: f64 = 0.4;
const KAPPA: f64 = 0.552_284_749_830_793_6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TikzParseResult {
    pub scene: GraphicsScene,
    pub diagnostics: Vec<TikzDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TikzDiagnostic {
    UnsupportedCommand { command: String },
    ParseError { message: String },
}

pub fn parse_tikzpicture(content: &str) -> TikzParseResult {
    let mut scene = GraphicsScene::default();
    let mut diagnostics = Vec::new();

    for statement in split_statements(content) {
        parse_statement(statement.trim(), &mut scene, &mut diagnostics);
    }

    TikzParseResult { scene, diagnostics }
}

fn parse_statement(
    statement: &str,
    scene: &mut GraphicsScene,
    diagnostics: &mut Vec<TikzDiagnostic>,
) {
    if statement.is_empty() {
        return;
    }

    let Some(rest) = statement.strip_prefix('\\') else {
        emit_parse_error(
            diagnostics,
            format!("expected tikz command, found `{statement}`"),
        );
        return;
    };

    let command_len = rest
        .chars()
        .take_while(|ch| ch.is_ascii_alphabetic())
        .map(char::len_utf8)
        .sum::<usize>();
    if command_len == 0 {
        emit_parse_error(
            diagnostics,
            format!("missing command name in `{statement}`"),
        );
        return;
    }

    let command = &rest[..command_len];
    let remainder = &rest[command_len..];
    match command {
        "draw" => parse_path_statement(
            remainder,
            Some(named_color("black")),
            None,
            scene,
            diagnostics,
        ),
        "fill" => parse_path_statement(
            remainder,
            None,
            Some(named_color("black")),
            scene,
            diagnostics,
        ),
        "filldraw" => parse_path_statement(
            remainder,
            Some(named_color("black")),
            Some(named_color("black")),
            scene,
            diagnostics,
        ),
        "node" => parse_node_statement(remainder, scene, diagnostics),
        unsupported => emit_unsupported(diagnostics, unsupported.to_string()),
    }
}

fn parse_path_statement(
    statement: &str,
    default_stroke: Option<Color>,
    default_fill: Option<Color>,
    scene: &mut GraphicsScene,
    diagnostics: &mut Vec<TikzDiagnostic>,
) {
    let mut cursor = Cursor::new(statement);
    let Some(options) = cursor.parse_optional_bracket_group() else {
        emit_parse_error(diagnostics, "unterminated tikz option list".to_string());
        return;
    };
    let Some(style) = PathStyle::parse(
        options.as_deref(),
        default_stroke,
        default_fill,
        diagnostics,
    ) else {
        return;
    };

    cursor.skip_whitespace();
    let Some(start) = cursor.parse_point() else {
        emit_parse_error(diagnostics, "expected starting coordinate".to_string());
        return;
    };

    cursor.skip_whitespace();
    let path = if cursor.consume_keyword("rectangle") {
        cursor.skip_whitespace();
        let Some(end) = cursor.parse_point() else {
            emit_parse_error(
                diagnostics,
                "rectangle requires an end coordinate".to_string(),
            );
            return;
        };
        rectangle_path(start, end)
    } else if cursor.consume_keyword("circle") {
        cursor.skip_whitespace();
        let Some(radius) = cursor.parse_circle_radius() else {
            emit_parse_error(diagnostics, "circle requires a radius".to_string());
            return;
        };
        circle_path(start, radius)
    } else {
        let mut path = vec![PathSegment::MoveTo(start)];
        loop {
            cursor.skip_whitespace();
            if cursor.is_eof() {
                break path;
            }
            if !cursor.consume_prefix("--") {
                emit_parse_error(
                    diagnostics,
                    format!(
                        "unsupported path segment near `{}`",
                        cursor.remaining().trim()
                    ),
                );
                return;
            }

            cursor.skip_whitespace();
            if cursor.consume_keyword("cycle") {
                path.push(PathSegment::ClosePath);
                break path;
            }

            let Some(point) = cursor.parse_point() else {
                emit_parse_error(diagnostics, "expected coordinate after `--`".to_string());
                return;
            };
            path.push(PathSegment::LineTo(point));
        }
    };

    cursor.skip_whitespace();
    if !cursor.is_eof() {
        emit_parse_error(
            diagnostics,
            format!(
                "could not parse tikz path tail `{}`",
                cursor.remaining().trim()
            ),
        );
        return;
    }

    scene.nodes.push(GraphicNode::Vector(VectorPrimitive {
        path,
        stroke: style.stroke,
        fill: style.fill,
        line_width: style.line_width,
    }));
}

fn parse_node_statement(
    statement: &str,
    scene: &mut GraphicsScene,
    diagnostics: &mut Vec<TikzDiagnostic>,
) {
    let mut cursor = Cursor::new(statement);
    let Some(options) = cursor.parse_optional_bracket_group() else {
        emit_parse_error(diagnostics, "unterminated tikz option list".to_string());
        return;
    };
    if let Some(options) = options.as_deref() {
        for option in split_options(options) {
            if !option.is_empty() {
                emit_unsupported(diagnostics, option.to_string());
                return;
            }
        }
    }

    cursor.skip_whitespace();
    if !cursor.consume_keyword("at") {
        emit_parse_error(diagnostics, "node requires `at (x,y)`".to_string());
        return;
    }

    cursor.skip_whitespace();
    let Some(position) = cursor.parse_point() else {
        emit_parse_error(diagnostics, "node requires a coordinate".to_string());
        return;
    };

    cursor.skip_whitespace();
    let Some(content) = cursor.parse_braced_text() else {
        emit_parse_error(diagnostics, "node requires braced text".to_string());
        return;
    };

    cursor.skip_whitespace();
    if !cursor.is_eof() {
        emit_parse_error(
            diagnostics,
            format!("unexpected node tail `{}`", cursor.remaining().trim()),
        );
        return;
    }

    scene
        .nodes
        .push(GraphicNode::Text(GraphicText { position, content }));
}

fn rectangle_path(start: Point, end: Point) -> Vec<PathSegment> {
    vec![
        PathSegment::MoveTo(start),
        PathSegment::LineTo(Point {
            x: end.x,
            y: start.y,
        }),
        PathSegment::LineTo(end),
        PathSegment::LineTo(Point {
            x: start.x,
            y: end.y,
        }),
        PathSegment::ClosePath,
    ]
}

fn circle_path(center: Point, radius: f64) -> Vec<PathSegment> {
    let control = radius * KAPPA;

    vec![
        PathSegment::MoveTo(Point {
            x: center.x + radius,
            y: center.y,
        }),
        PathSegment::CurveTo {
            control1: Point {
                x: center.x + radius,
                y: center.y + control,
            },
            control2: Point {
                x: center.x + control,
                y: center.y + radius,
            },
            end: Point {
                x: center.x,
                y: center.y + radius,
            },
        },
        PathSegment::CurveTo {
            control1: Point {
                x: center.x - control,
                y: center.y + radius,
            },
            control2: Point {
                x: center.x - radius,
                y: center.y + control,
            },
            end: Point {
                x: center.x - radius,
                y: center.y,
            },
        },
        PathSegment::CurveTo {
            control1: Point {
                x: center.x - radius,
                y: center.y - control,
            },
            control2: Point {
                x: center.x - control,
                y: center.y - radius,
            },
            end: Point {
                x: center.x,
                y: center.y - radius,
            },
        },
        PathSegment::CurveTo {
            control1: Point {
                x: center.x + control,
                y: center.y - radius,
            },
            control2: Point {
                x: center.x + radius,
                y: center.y - control,
            },
            end: Point {
                x: center.x + radius,
                y: center.y,
            },
        },
        PathSegment::ClosePath,
    ]
}

fn split_statements(content: &str) -> Vec<&str> {
    let mut statements = Vec::new();
    let mut start = 0usize;
    let mut brace_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut paren_depth = 0usize;

    for (index, ch) in content.char_indices() {
        match ch {
            '{' => brace_depth += 1,
            '}' => brace_depth = brace_depth.saturating_sub(1),
            '[' => bracket_depth += 1,
            ']' => bracket_depth = bracket_depth.saturating_sub(1),
            '(' => paren_depth += 1,
            ')' => paren_depth = paren_depth.saturating_sub(1),
            ';' if brace_depth == 0 && bracket_depth == 0 && paren_depth == 0 => {
                statements.push(&content[start..index]);
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }

    if start < content.len() {
        statements.push(&content[start..]);
    }

    statements
}

fn split_options(options: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut start = 0usize;
    let mut brace_depth = 0usize;
    let mut paren_depth = 0usize;

    for (index, ch) in options.char_indices() {
        match ch {
            '{' => brace_depth += 1,
            '}' => brace_depth = brace_depth.saturating_sub(1),
            '(' => paren_depth += 1,
            ')' => paren_depth = paren_depth.saturating_sub(1),
            ',' if brace_depth == 0 && paren_depth == 0 => {
                result.push(options[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    result.push(options[start..].trim());
    result
}

#[derive(Debug, Clone, Copy)]
struct PathStyle {
    stroke: Option<Color>,
    fill: Option<Color>,
    line_width: f64,
}

impl PathStyle {
    fn parse(
        options: Option<&str>,
        default_stroke: Option<Color>,
        default_fill: Option<Color>,
        diagnostics: &mut Vec<TikzDiagnostic>,
    ) -> Option<Self> {
        let mut style = Self {
            stroke: default_stroke,
            fill: default_fill,
            line_width: DEFAULT_LINE_WIDTH_PT,
        };

        let Some(options) = options else {
            return Some(style);
        };

        for option in split_options(options) {
            if option.is_empty() {
                continue;
            }

            let Some((key, value)) = option
                .split_once('=')
                .map(|(key, value)| (key.trim(), Some(value.trim())))
                .or_else(|| Some((option.trim(), None)))
            else {
                continue;
            };

            match (key, value) {
                ("draw", None) => style.stroke = Some(named_color("black")),
                ("fill", None) => style.fill = Some(named_color("black")),
                ("draw", Some(color_name)) => {
                    let Some(color) = resolve_named_color(color_name) else {
                        emit_unsupported(diagnostics, option.to_string());
                        return None;
                    };
                    style.stroke = Some(color);
                }
                ("fill", Some(color_name)) => {
                    let Some(color) = resolve_named_color(color_name) else {
                        emit_unsupported(diagnostics, option.to_string());
                        return None;
                    };
                    style.fill = Some(color);
                }
                ("line width", Some(value)) => {
                    let Some(line_width) = parse_length(value, false) else {
                        emit_parse_error(diagnostics, format!("invalid line width `{value}`"));
                        return None;
                    };
                    style.line_width = line_width;
                }
                (color_name, None) => {
                    let Some(color) = resolve_named_color(color_name) else {
                        emit_unsupported(diagnostics, option.to_string());
                        return None;
                    };
                    match (default_stroke.is_some(), default_fill.is_some()) {
                        (true, true) => {
                            style.stroke = Some(color);
                            style.fill = Some(color);
                        }
                        (true, false) => style.stroke = Some(color),
                        (false, true) => style.fill = Some(color),
                        (false, false) => {}
                    }
                }
                _ => {
                    emit_unsupported(diagnostics, option.to_string());
                    return None;
                }
            }
        }

        Some(style)
    }
}

struct Cursor<'a> {
    input: &'a str,
    index: usize,
}

impl<'a> Cursor<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, index: 0 }
    }

    fn remaining(&self) -> &'a str {
        &self.input[self.index..]
    }

    fn is_eof(&self) -> bool {
        self.index >= self.input.len()
    }

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.remaining().chars().next() {
            if !ch.is_whitespace() {
                break;
            }
            self.index += ch.len_utf8();
        }
    }

    fn consume_prefix(&mut self, prefix: &str) -> bool {
        if self.remaining().starts_with(prefix) {
            self.index += prefix.len();
            true
        } else {
            false
        }
    }

    fn consume_keyword(&mut self, keyword: &str) -> bool {
        let rest = self.remaining();
        if !rest.starts_with(keyword) {
            return false;
        }

        if let Some(next) = rest[keyword.len()..].chars().next() {
            if next.is_ascii_alphabetic() {
                return false;
            }
        }

        self.index += keyword.len();
        true
    }

    fn parse_optional_bracket_group(&mut self) -> Option<Option<String>> {
        self.skip_whitespace();
        if !self.consume_prefix("[") {
            return Some(None);
        }

        let start = self.index;
        let mut depth = 1usize;
        while self.index < self.input.len() {
            let ch = self.remaining().chars().next()?;
            self.index += ch.len_utf8();
            match ch {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        let end = self.index - 1;
                        return Some(Some(self.input[start..end].to_string()));
                    }
                }
                _ => {}
            }
        }

        None
    }

    fn parse_point(&mut self) -> Option<Point> {
        self.skip_whitespace();
        if !self.consume_prefix("(") {
            return None;
        }

        self.skip_whitespace();
        let x = self.parse_length_token(true)?;
        self.skip_whitespace();
        if !self.consume_prefix(",") {
            return None;
        }
        self.skip_whitespace();
        let y = self.parse_length_token(true)?;
        self.skip_whitespace();
        if !self.consume_prefix(")") {
            return None;
        }

        Some(Point { x, y })
    }

    fn parse_circle_radius(&mut self) -> Option<f64> {
        self.skip_whitespace();
        if !self.consume_prefix("(") {
            return None;
        }
        self.skip_whitespace();
        let radius = self.parse_length_token(true)?;
        self.skip_whitespace();
        if !self.consume_prefix(")") {
            return None;
        }
        Some(radius)
    }

    fn parse_braced_text(&mut self) -> Option<String> {
        self.skip_whitespace();
        if !self.consume_prefix("{") {
            return None;
        }

        let start = self.index;
        let mut depth = 1usize;
        while self.index < self.input.len() {
            let ch = self.remaining().chars().next()?;
            self.index += ch.len_utf8();
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        let end = self.index - 1;
                        return Some(self.input[start..end].to_string());
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn parse_length_token(&mut self, default_cm: bool) -> Option<f64> {
        let rest = self.remaining();
        let token_len = rest
            .char_indices()
            .take_while(|(_, ch)| !ch.is_whitespace() && *ch != ',' && *ch != ')')
            .map(|(index, ch)| index + ch.len_utf8())
            .last()
            .unwrap_or(0);
        if token_len == 0 {
            return None;
        }
        let token = &rest[..token_len];
        let value = parse_length(token, default_cm)?;
        self.index += token_len;
        Some(value)
    }
}

fn parse_length(token: &str, default_cm: bool) -> Option<f64> {
    let token = token.trim();
    let (number, unit_factor) = if let Some(number) = token.strip_suffix("cm") {
        (number, CM_IN_PT)
    } else if let Some(number) = token.strip_suffix("pt") {
        (number, 1.0)
    } else {
        (token, if default_cm { CM_IN_PT } else { return None })
    };
    let value = number.trim().parse::<f64>().ok()?;
    value.is_finite().then_some(value * unit_factor)
}

fn resolve_named_color(name: &str) -> Option<Color> {
    match name.trim() {
        "black" => Some(named_color("black")),
        "white" => Some(named_color("white")),
        "red" => Some(named_color("red")),
        "green" => Some(named_color("green")),
        "blue" => Some(named_color("blue")),
        _ => None,
    }
}

fn named_color(name: &str) -> Color {
    match name {
        "white" => Color {
            r: 1.0,
            g: 1.0,
            b: 1.0,
        },
        "red" => Color {
            r: 1.0,
            g: 0.0,
            b: 0.0,
        },
        "green" => Color {
            r: 0.0,
            g: 1.0,
            b: 0.0,
        },
        "blue" => Color {
            r: 0.0,
            g: 0.0,
            b: 1.0,
        },
        _ => Color {
            r: 0.0,
            g: 0.0,
            b: 0.0,
        },
    }
}

fn emit_unsupported(diagnostics: &mut Vec<TikzDiagnostic>, command: String) {
    diagnostics.push(TikzDiagnostic::UnsupportedCommand { command });
}

fn emit_parse_error(diagnostics: &mut Vec<TikzDiagnostic>, message: String) {
    diagnostics.push(TikzDiagnostic::ParseError { message });
}

#[cfg(test)]
mod tests {
    use crate::graphics::api::{
        compile_graphics_scene, GraphicNode, GraphicText, GraphicsScene, PathSegment, Point,
    };

    use super::{circle_path, named_color, parse_tikzpicture, TikzDiagnostic, CM_IN_PT, KAPPA};

    fn assert_point_close(actual: Point, expected: Point) {
        assert!(
            (actual.x - expected.x).abs() < 1e-9,
            "x mismatch: {actual:?} != {expected:?}"
        );
        assert!(
            (actual.y - expected.y).abs() < 1e-9,
            "y mismatch: {actual:?} != {expected:?}"
        );
    }

    #[test]
    fn parses_draw_path_with_cycle() {
        let result = parse_tikzpicture(r"\draw (0,0) -- (1,0) -- (1,1) -- cycle;");

        assert!(result.diagnostics.is_empty());
        assert_eq!(
            result.scene.nodes,
            vec![GraphicNode::Vector(crate::graphics::api::VectorPrimitive {
                path: vec![
                    PathSegment::MoveTo(Point { x: 0.0, y: 0.0 }),
                    PathSegment::LineTo(Point {
                        x: CM_IN_PT,
                        y: 0.0,
                    }),
                    PathSegment::LineTo(Point {
                        x: CM_IN_PT,
                        y: CM_IN_PT,
                    }),
                    PathSegment::ClosePath,
                ],
                stroke: Some(named_color("black")),
                fill: None,
                line_width: 0.4,
            })]
        );
    }

    #[test]
    fn parses_rectangle_shorthand() {
        let result = parse_tikzpicture(r"\draw (0,0) rectangle (2,1);");

        assert!(result.diagnostics.is_empty());
        assert_eq!(
            result.scene.nodes,
            vec![GraphicNode::Vector(crate::graphics::api::VectorPrimitive {
                path: vec![
                    PathSegment::MoveTo(Point { x: 0.0, y: 0.0 }),
                    PathSegment::LineTo(Point {
                        x: 2.0 * CM_IN_PT,
                        y: 0.0,
                    }),
                    PathSegment::LineTo(Point {
                        x: 2.0 * CM_IN_PT,
                        y: CM_IN_PT,
                    }),
                    PathSegment::LineTo(Point {
                        x: 0.0,
                        y: CM_IN_PT,
                    }),
                    PathSegment::ClosePath,
                ],
                stroke: Some(named_color("black")),
                fill: None,
                line_width: 0.4,
            })]
        );
    }

    #[test]
    fn parses_filled_circle() {
        let result = parse_tikzpicture(r"\fill[red] (0,0) circle (0.5cm);");

        assert!(result.diagnostics.is_empty());
        let GraphicNode::Vector(primitive) = &result.scene.nodes[0] else {
            panic!("expected vector node");
        };
        assert_eq!(primitive.fill, Some(named_color("red")));
        assert_eq!(primitive.stroke, None);
        assert_eq!(primitive.path.len(), 6);
        assert!(matches!(
            primitive.path.first(),
            Some(PathSegment::MoveTo(_))
        ));
        assert!(matches!(
            primitive.path.get(1),
            Some(PathSegment::CurveTo { .. })
        ));
        assert!(matches!(
            primitive.path.get(2),
            Some(PathSegment::CurveTo { .. })
        ));
        assert!(matches!(
            primitive.path.get(3),
            Some(PathSegment::CurveTo { .. })
        ));
        assert!(matches!(
            primitive.path.get(4),
            Some(PathSegment::CurveTo { .. })
        ));
        assert!(matches!(
            primitive.path.last(),
            Some(PathSegment::ClosePath)
        ));
    }

    #[test]
    fn circle_path_uses_cubic_beziers() {
        let path = circle_path(Point { x: 10.0, y: 20.0 }, 5.0);

        assert_eq!(path.len(), 6);
        assert_eq!(path[0], PathSegment::MoveTo(Point { x: 15.0, y: 20.0 }));
        let PathSegment::CurveTo {
            control1,
            control2,
            end,
        } = path[1]
        else {
            panic!("expected first arc to be cubic bezier");
        };
        assert_point_close(
            control1,
            Point {
                x: 15.0,
                y: 20.0 + 5.0 * KAPPA,
            },
        );
        assert_point_close(
            control2,
            Point {
                x: 10.0 + 5.0 * KAPPA,
                y: 25.0,
            },
        );
        assert_eq!(end, Point { x: 10.0, y: 25.0 });
        assert!(matches!(path[2], PathSegment::CurveTo { .. }));
        assert!(matches!(path[3], PathSegment::CurveTo { .. }));
        assert!(matches!(path[4], PathSegment::CurveTo { .. }));
        assert_eq!(path[5], PathSegment::ClosePath);
    }

    #[test]
    fn parses_filldraw_with_named_colors() {
        let result = parse_tikzpicture(r"\filldraw[draw=black,fill=blue] (0,0) rectangle (1,1);");

        assert!(result.diagnostics.is_empty());
        let GraphicNode::Vector(primitive) = &result.scene.nodes[0] else {
            panic!("expected vector node");
        };
        assert_eq!(primitive.stroke, Some(named_color("black")));
        assert_eq!(primitive.fill, Some(named_color("blue")));
    }

    #[test]
    fn parses_text_node() {
        let result = parse_tikzpicture(r"\node at (1,1) {Hello};");

        assert_eq!(
            result.scene.nodes,
            vec![GraphicNode::Text(GraphicText {
                position: Point {
                    x: CM_IN_PT,
                    y: CM_IN_PT,
                },
                content: "Hello".to_string(),
            })]
        );
    }

    #[test]
    fn reports_unsupported_command_without_crashing() {
        let result = parse_tikzpicture(r"\clip (0,0) rectangle (1,1);");

        assert!(result.scene.nodes.is_empty());
        assert_eq!(
            result.diagnostics,
            vec![TikzDiagnostic::UnsupportedCommand {
                command: "clip".to_string(),
            }]
        );
    }

    #[test]
    fn parses_empty_tikzpicture() {
        let result = parse_tikzpicture(" \n ");

        assert_eq!(result.scene, GraphicsScene::default());
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn compile_graphics_scene_from_tikz_result_uses_bounds() {
        let parsed = parse_tikzpicture("\\draw (1,2) rectangle (3,4);");
        let graphics_box = compile_graphics_scene(parsed.scene);

        assert_eq!(
            graphics_box.width.0,
            (2.0 * CM_IN_PT * 65_536.0).round() as i64
        );
        assert_eq!(
            graphics_box.height.0,
            (2.0 * CM_IN_PT * 65_536.0).round() as i64
        );
    }
}
