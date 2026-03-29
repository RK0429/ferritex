use crate::graphics::api::{
    ArrowSpec, Color, GraphicGroup, GraphicNode, GraphicText, GraphicsScene, PathSegment, Point,
    Transform2D, VectorPrimitive,
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
    let mut diagnostics = Vec::new();
    let scene = GraphicsScene {
        nodes: materialize_statements(parse_scope_items(
            content,
            ScopeState::default(),
            &mut diagnostics,
        )),
    };

    TikzParseResult { scene, diagnostics }
}

#[derive(Debug, Clone, Copy)]
struct ScopeState {
    default_stroke: Option<Color>,
    default_fill: Option<Color>,
    default_line_width: Option<f64>,
    transform: Transform2D,
}

impl Default for ScopeState {
    fn default() -> Self {
        Self {
            default_stroke: None,
            default_fill: None,
            default_line_width: Some(DEFAULT_LINE_WIDTH_PT),
            transform: Transform2D::default(),
        }
    }
}

#[derive(Debug, Clone)]
enum ParsedStatement {
    Node(GraphicNode),
    Clip(ClipGroupSpec),
}

#[derive(Debug, Clone)]
struct ClipGroupSpec {
    default_stroke: Option<Color>,
    default_fill: Option<Color>,
    default_line_width: Option<f64>,
    clip_path: Vec<PathSegment>,
    transform: Transform2D,
}

#[derive(Debug, Clone, Copy)]
enum PathCommandKind {
    Draw,
    Fill,
    FillDraw,
    Clip,
}

#[derive(Debug, Clone, Copy)]
struct PathStyle {
    stroke: Option<Color>,
    fill: Option<Color>,
    line_width: f64,
    transform: Transform2D,
    arrows: ArrowSpec,
}

fn parse_scope_items(
    content: &str,
    state: ScopeState,
    diagnostics: &mut Vec<TikzDiagnostic>,
) -> Vec<ParsedStatement> {
    let mut items = Vec::new();
    let mut cursor = Cursor::new(content);

    while !cursor.is_eof() {
        cursor.skip_whitespace();
        if cursor.is_eof() {
            break;
        }

        if cursor.remaining().starts_with("\\begin") {
            if let Some(group) = parse_scope_block(&mut cursor, state, diagnostics) {
                items.push(ParsedStatement::Node(GraphicNode::Group(group)));
            }
            continue;
        }

        let Some(statement) = cursor.parse_statement_chunk() else {
            break;
        };
        parse_statement(statement.trim(), state, &mut items, diagnostics);
    }

    items
}

fn materialize_statements(statements: Vec<ParsedStatement>) -> Vec<GraphicNode> {
    let mut nodes = Vec::new();
    let mut iter = statements.into_iter();
    while let Some(statement) = iter.next() {
        match statement {
            ParsedStatement::Node(node) => nodes.push(node),
            ParsedStatement::Clip(clip) => {
                nodes.push(GraphicNode::Group(GraphicGroup {
                    children: materialize_statements(iter.collect()),
                    default_stroke: clip.default_stroke,
                    default_fill: clip.default_fill,
                    default_line_width: clip.default_line_width,
                    clip_path: Some(clip.clip_path),
                    transform: clip.transform,
                }));
                break;
            }
        }
    }
    nodes
}

fn parse_scope_block(
    cursor: &mut Cursor<'_>,
    parent_state: ScopeState,
    diagnostics: &mut Vec<TikzDiagnostic>,
) -> Option<GraphicGroup> {
    if !cursor.consume_prefix("\\begin") {
        return None;
    }

    cursor.skip_whitespace();
    let Some(environment) = cursor.parse_braced_text() else {
        emit_parse_error(
            diagnostics,
            "begin requires braced environment name".to_string(),
        );
        cursor.index = cursor.input.len();
        return None;
    };
    if environment != "scope" {
        emit_unsupported(diagnostics, format!("begin{{{environment}}}"));
        cursor.index = cursor.input.len();
        return None;
    }

    let Some(options) = cursor.parse_optional_bracket_group() else {
        emit_parse_error(diagnostics, "unterminated scope option list".to_string());
        cursor.index = cursor.input.len();
        return None;
    };

    let body_start = cursor.index;
    let Some((body_end, scope_end)) = find_matching_scope_end(cursor.input, body_start) else {
        emit_parse_error(diagnostics, "unterminated scope environment".to_string());
        cursor.index = cursor.input.len();
        return None;
    };
    cursor.index = scope_end;

    let scope_options = ScopeOptions::parse(options.as_deref(), parent_state, diagnostics);
    let child_state = ScopeState {
        default_stroke: scope_options.default_stroke,
        default_fill: scope_options.default_fill,
        default_line_width: scope_options.default_line_width,
        transform: parent_state.transform.compose(scope_options.transform),
    };

    Some(GraphicGroup {
        children: materialize_statements(parse_scope_items(
            &cursor.input[body_start..body_end],
            child_state,
            diagnostics,
        )),
        default_stroke: scope_options.default_stroke,
        default_fill: scope_options.default_fill,
        default_line_width: scope_options.default_line_width,
        clip_path: None,
        transform: scope_options.transform,
    })
}

fn find_matching_scope_end(input: &str, start: usize) -> Option<(usize, usize)> {
    let mut depth = 1usize;
    let mut index = start;
    let begin_token = "\\begin{scope}";
    let end_token = "\\end{scope}";

    while index < input.len() {
        let remaining = &input[index..];
        let next_begin = remaining.find(begin_token).map(|offset| index + offset);
        let next_end = remaining.find(end_token).map(|offset| index + offset);

        match (next_begin, next_end) {
            (Some(begin), Some(end)) if begin < end => {
                depth += 1;
                index = begin + begin_token.len();
            }
            (_, Some(end)) => {
                depth -= 1;
                if depth == 0 {
                    return Some((end, end + end_token.len()));
                }
                index = end + end_token.len();
            }
            (Some(begin), None) => {
                depth += 1;
                index = begin + begin_token.len();
            }
            (None, None) => break,
        }
    }

    None
}

fn parse_statement(
    statement: &str,
    state: ScopeState,
    items: &mut Vec<ParsedStatement>,
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
        "draw" => {
            if let Some(node) =
                parse_path_statement(remainder, state, PathCommandKind::Draw, diagnostics)
            {
                items.push(ParsedStatement::Node(node));
            }
        }
        "fill" => {
            if let Some(node) =
                parse_path_statement(remainder, state, PathCommandKind::Fill, diagnostics)
            {
                items.push(ParsedStatement::Node(node));
            }
        }
        "filldraw" => {
            if let Some(node) =
                parse_path_statement(remainder, state, PathCommandKind::FillDraw, diagnostics)
            {
                items.push(ParsedStatement::Node(node));
            }
        }
        "clip" => {
            if let Some(clip) = parse_clip_statement(remainder, state, diagnostics) {
                items.push(ParsedStatement::Clip(clip));
            }
        }
        "node" => {
            if let Some(node) = parse_node_statement(remainder, state, diagnostics) {
                items.push(ParsedStatement::Node(node));
            }
        }
        unsupported => emit_unsupported(diagnostics, unsupported.to_string()),
    }
}

fn parse_path_statement(
    statement: &str,
    state: ScopeState,
    command_kind: PathCommandKind,
    diagnostics: &mut Vec<TikzDiagnostic>,
) -> Option<GraphicNode> {
    let mut cursor = Cursor::new(statement);
    let Some(options) = cursor.parse_optional_bracket_group() else {
        emit_parse_error(diagnostics, "unterminated tikz option list".to_string());
        return None;
    };

    let mut style = PathStyle::parse(options.as_deref(), state, command_kind, diagnostics);
    cursor.skip_whitespace();
    if let Some(inline_arrows) = cursor.consume_arrow_spec() {
        style.arrows = inline_arrows;
        cursor.skip_whitespace();
    }

    let Some(path) = parse_path_segments(&mut cursor, diagnostics) else {
        return None;
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
        return None;
    }

    Some(wrap_with_transform(
        GraphicNode::Vector(VectorPrimitive {
            path,
            stroke: style.stroke,
            fill: style.fill,
            line_width: style.line_width,
            arrows: style.arrows,
        }),
        state,
        style.transform,
    ))
}

fn parse_clip_statement(
    statement: &str,
    state: ScopeState,
    diagnostics: &mut Vec<TikzDiagnostic>,
) -> Option<ClipGroupSpec> {
    let mut cursor = Cursor::new(statement);
    let Some(options) = cursor.parse_optional_bracket_group() else {
        emit_parse_error(diagnostics, "unterminated tikz option list".to_string());
        return None;
    };

    let style = PathStyle::parse(
        options.as_deref(),
        state,
        PathCommandKind::Clip,
        diagnostics,
    );
    let path = parse_path_segments(&mut cursor, diagnostics)?;

    cursor.skip_whitespace();
    if !cursor.is_eof() {
        emit_parse_error(
            diagnostics,
            format!(
                "could not parse tikz path tail `{}`",
                cursor.remaining().trim()
            ),
        );
        return None;
    }

    Some(ClipGroupSpec {
        default_stroke: state.default_stroke,
        default_fill: state.default_fill,
        default_line_width: state.default_line_width,
        clip_path: path,
        transform: style.transform,
    })
}

fn parse_node_statement(
    statement: &str,
    state: ScopeState,
    diagnostics: &mut Vec<TikzDiagnostic>,
) -> Option<GraphicNode> {
    let mut cursor = Cursor::new(statement);
    let Some(options) = cursor.parse_optional_bracket_group() else {
        emit_parse_error(diagnostics, "unterminated tikz option list".to_string());
        return None;
    };
    let transform = parse_node_options(options.as_deref(), diagnostics);

    cursor.skip_whitespace();
    if !cursor.consume_keyword("at") {
        emit_parse_error(diagnostics, "node requires `at (x,y)`".to_string());
        return None;
    }

    cursor.skip_whitespace();
    let Some(position) = cursor.parse_point() else {
        emit_parse_error(diagnostics, "node requires a coordinate".to_string());
        return None;
    };

    cursor.skip_whitespace();
    let Some(content) = cursor.parse_braced_text() else {
        emit_parse_error(diagnostics, "node requires braced text".to_string());
        return None;
    };

    cursor.skip_whitespace();
    if !cursor.is_eof() {
        emit_parse_error(
            diagnostics,
            format!("unexpected node tail `{}`", cursor.remaining().trim()),
        );
        return None;
    }

    Some(wrap_with_transform(
        GraphicNode::Text(GraphicText { position, content }),
        state,
        transform,
    ))
}

fn parse_path_segments(
    cursor: &mut Cursor<'_>,
    diagnostics: &mut Vec<TikzDiagnostic>,
) -> Option<Vec<PathSegment>> {
    cursor.skip_whitespace();
    let Some(start) = cursor.parse_point() else {
        emit_parse_error(diagnostics, "expected starting coordinate".to_string());
        return None;
    };

    cursor.skip_whitespace();
    if cursor.consume_keyword("rectangle") {
        cursor.skip_whitespace();
        let Some(end) = cursor.parse_point() else {
            emit_parse_error(
                diagnostics,
                "rectangle requires an end coordinate".to_string(),
            );
            return None;
        };
        return Some(rectangle_path(start, end));
    }
    if cursor.consume_keyword("circle") {
        cursor.skip_whitespace();
        let Some(radius) = cursor.parse_circle_radius() else {
            emit_parse_error(diagnostics, "circle requires a radius".to_string());
            return None;
        };
        return Some(circle_path(start, radius));
    }

    let mut path = vec![PathSegment::MoveTo(start)];
    loop {
        cursor.skip_whitespace();
        if cursor.is_eof() {
            return Some(path);
        }
        if !cursor.consume_prefix("--") {
            emit_parse_error(
                diagnostics,
                format!(
                    "unsupported path segment near `{}`",
                    cursor.remaining().trim()
                ),
            );
            return None;
        }

        cursor.skip_whitespace();
        if cursor.consume_keyword("cycle") {
            path.push(PathSegment::ClosePath);
            return Some(path);
        }

        let Some(point) = cursor.parse_point() else {
            emit_parse_error(diagnostics, "expected coordinate after `--`".to_string());
            return None;
        };
        path.push(PathSegment::LineTo(point));
    }
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

impl PathStyle {
    fn parse(
        options: Option<&str>,
        state: ScopeState,
        command_kind: PathCommandKind,
        diagnostics: &mut Vec<TikzDiagnostic>,
    ) -> Self {
        let mut style = Self {
            stroke: state.default_stroke,
            fill: state.default_fill,
            line_width: state.default_line_width.unwrap_or(DEFAULT_LINE_WIDTH_PT),
            transform: Transform2D::default(),
            arrows: ArrowSpec::None,
        };

        if command_kind.uses_stroke() && style.stroke.is_none() {
            style.stroke = Some(named_color("black"));
        }
        if command_kind.uses_fill() && style.fill.is_none() {
            style.fill = Some(named_color("black"));
        }

        let Some(options) = options else {
            return style;
        };

        for option in split_options(options) {
            if option.is_empty() {
                continue;
            }

            if let Some(arrows) = parse_arrow_option(option) {
                style.arrows = arrows;
                continue;
            }

            match split_option(option) {
                ("draw", None) => style.stroke = Some(named_color("black")),
                ("fill", None) => style.fill = Some(named_color("black")),
                ("draw", Some(color_name)) => {
                    if let Some(color) = resolve_named_color(color_name) {
                        style.stroke = Some(color);
                    } else {
                        emit_unsupported(diagnostics, option.to_string());
                    }
                }
                ("fill", Some(color_name)) => {
                    if let Some(color) = resolve_named_color(color_name) {
                        style.fill = Some(color);
                    } else {
                        emit_unsupported(diagnostics, option.to_string());
                    }
                }
                ("line width", Some(value)) => {
                    let Some(line_width) = parse_length(value, false) else {
                        emit_parse_error(diagnostics, format!("invalid line width `{value}`"));
                        continue;
                    };
                    style.line_width = line_width;
                }
                ("xshift", Some(value)) => {
                    if let Some(length) = parse_length(value, false) {
                        style.transform.x_shift = length;
                    } else {
                        emit_parse_error(diagnostics, format!("invalid xshift `{value}`"));
                    }
                }
                ("yshift", Some(value)) => {
                    if let Some(length) = parse_length(value, false) {
                        style.transform.y_shift = length;
                    } else {
                        emit_parse_error(diagnostics, format!("invalid yshift `{value}`"));
                    }
                }
                ("shift", Some(value)) => match parse_shift(value) {
                    Some(point) => {
                        style.transform.x_shift = point.x;
                        style.transform.y_shift = point.y;
                    }
                    None => emit_parse_error(diagnostics, format!("invalid shift `{value}`")),
                },
                ("scale", Some(value)) => match value.parse::<f64>() {
                    Ok(scale) if scale.is_finite() => style.transform.scale = scale,
                    _ => emit_parse_error(diagnostics, format!("invalid scale `{value}`")),
                },
                ("rotate", Some(value)) => match value.parse::<f64>() {
                    Ok(rotate) if rotate.is_finite() => style.transform.rotate = rotate,
                    _ => emit_parse_error(diagnostics, format!("invalid rotate `{value}`")),
                },
                (color_name, None) => {
                    if let Some(color) = resolve_named_color(color_name) {
                        match (style.stroke.is_some(), style.fill.is_some()) {
                            (true, true) => {
                                style.stroke = Some(color);
                                style.fill = Some(color);
                            }
                            (true, false) => style.stroke = Some(color),
                            (false, true) => style.fill = Some(color),
                            (false, false) => {}
                        }
                    } else {
                        emit_unsupported(diagnostics, option.to_string());
                    }
                }
                _ => emit_unsupported(diagnostics, option.to_string()),
            }
        }

        style
    }
}

impl PathCommandKind {
    fn uses_stroke(self) -> bool {
        matches!(self, Self::Draw | Self::FillDraw)
    }

    fn uses_fill(self) -> bool {
        matches!(self, Self::Fill | Self::FillDraw)
    }
}

#[derive(Debug, Clone, Copy)]
struct ScopeOptions {
    default_stroke: Option<Color>,
    default_fill: Option<Color>,
    default_line_width: Option<f64>,
    transform: Transform2D,
}

impl ScopeOptions {
    fn parse(
        options: Option<&str>,
        parent: ScopeState,
        diagnostics: &mut Vec<TikzDiagnostic>,
    ) -> Self {
        let mut parsed = Self {
            default_stroke: parent.default_stroke,
            default_fill: parent.default_fill,
            default_line_width: parent.default_line_width,
            transform: Transform2D::default(),
        };

        let Some(options) = options else {
            return parsed;
        };

        for option in split_options(options) {
            if option.is_empty() {
                continue;
            }

            match split_option(option) {
                ("draw", None) => parsed.default_stroke = Some(named_color("black")),
                ("fill", None) => parsed.default_fill = Some(named_color("black")),
                ("draw", Some(color_name)) => {
                    if let Some(color) = resolve_named_color(color_name) {
                        parsed.default_stroke = Some(color);
                    } else {
                        emit_unsupported(diagnostics, option.to_string());
                    }
                }
                ("fill", Some(color_name)) => {
                    if let Some(color) = resolve_named_color(color_name) {
                        parsed.default_fill = Some(color);
                    } else {
                        emit_unsupported(diagnostics, option.to_string());
                    }
                }
                ("line width", Some(value)) => match parse_length(value, false) {
                    Some(line_width) => parsed.default_line_width = Some(line_width),
                    None => emit_parse_error(diagnostics, format!("invalid line width `{value}`")),
                },
                ("xshift", Some(value)) => match parse_length(value, false) {
                    Some(length) => parsed.transform.x_shift = length,
                    None => emit_parse_error(diagnostics, format!("invalid xshift `{value}`")),
                },
                ("yshift", Some(value)) => match parse_length(value, false) {
                    Some(length) => parsed.transform.y_shift = length,
                    None => emit_parse_error(diagnostics, format!("invalid yshift `{value}`")),
                },
                ("shift", Some(value)) => match parse_shift(value) {
                    Some(point) => {
                        parsed.transform.x_shift = point.x;
                        parsed.transform.y_shift = point.y;
                    }
                    None => emit_parse_error(diagnostics, format!("invalid shift `{value}`")),
                },
                ("scale", Some(value)) => match value.parse::<f64>() {
                    Ok(scale) if scale.is_finite() => parsed.transform.scale = scale,
                    _ => emit_parse_error(diagnostics, format!("invalid scale `{value}`")),
                },
                ("rotate", Some(value)) => match value.parse::<f64>() {
                    Ok(rotate) if rotate.is_finite() => parsed.transform.rotate = rotate,
                    _ => emit_parse_error(diagnostics, format!("invalid rotate `{value}`")),
                },
                (color_name, None) => {
                    if let Some(color) = resolve_named_color(color_name) {
                        parsed.default_stroke = Some(color);
                        parsed.default_fill = Some(color);
                    } else {
                        emit_unsupported(diagnostics, option.to_string());
                    }
                }
                _ => emit_unsupported(diagnostics, option.to_string()),
            }
        }

        parsed
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

    fn consume_arrow_spec(&mut self) -> Option<ArrowSpec> {
        for (token, arrows) in [
            ("<->", ArrowSpec::Both),
            ("->", ArrowSpec::Forward),
            ("<-", ArrowSpec::Backward),
        ] {
            if self.consume_prefix(token) {
                return Some(arrows);
            }
        }
        None
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

    fn parse_statement_chunk(&mut self) -> Option<&'a str> {
        let start = self.index;
        let mut brace_depth = 0usize;
        let mut bracket_depth = 0usize;
        let mut paren_depth = 0usize;

        while self.index < self.input.len() {
            let ch = self.remaining().chars().next()?;
            self.index += ch.len_utf8();
            match ch {
                '{' => brace_depth += 1,
                '}' => brace_depth = brace_depth.saturating_sub(1),
                '[' => bracket_depth += 1,
                ']' => bracket_depth = bracket_depth.saturating_sub(1),
                '(' => paren_depth += 1,
                ')' => paren_depth = paren_depth.saturating_sub(1),
                ';' if brace_depth == 0 && bracket_depth == 0 && paren_depth == 0 => {
                    return Some(&self.input[start..self.index - 1]);
                }
                _ => {}
            }
        }

        (start < self.input.len()).then(|| self.input[start..].trim_end())
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

fn parse_shift(value: &str) -> Option<Point> {
    let trimmed = value.trim();
    let coordinate = trimmed
        .strip_prefix('{')
        .and_then(|inner| inner.strip_suffix('}'))
        .unwrap_or(trimmed);
    let mut cursor = Cursor::new(coordinate);
    let point = cursor.parse_point()?;
    cursor.skip_whitespace();
    cursor.is_eof().then_some(point)
}

fn split_option(option: &str) -> (&str, Option<&str>) {
    option
        .split_once('=')
        .map(|(key, value)| (key.trim(), Some(value.trim())))
        .unwrap_or((option.trim(), None))
}

fn parse_arrow_option(option: &str) -> Option<ArrowSpec> {
    match option.trim() {
        "->" => Some(ArrowSpec::Forward),
        "<-" => Some(ArrowSpec::Backward),
        "<->" => Some(ArrowSpec::Both),
        _ => None,
    }
}

fn parse_node_options(options: Option<&str>, diagnostics: &mut Vec<TikzDiagnostic>) -> Transform2D {
    let mut transform = Transform2D::default();
    let Some(options) = options else {
        return transform;
    };

    for option in split_options(options) {
        if option.is_empty() {
            continue;
        }

        match split_option(option) {
            ("xshift", Some(value)) => match parse_length(value, false) {
                Some(length) => transform.x_shift = length,
                None => emit_parse_error(diagnostics, format!("invalid xshift `{value}`")),
            },
            ("yshift", Some(value)) => match parse_length(value, false) {
                Some(length) => transform.y_shift = length,
                None => emit_parse_error(diagnostics, format!("invalid yshift `{value}`")),
            },
            ("shift", Some(value)) => match parse_shift(value) {
                Some(point) => {
                    transform.x_shift = point.x;
                    transform.y_shift = point.y;
                }
                None => emit_parse_error(diagnostics, format!("invalid shift `{value}`")),
            },
            ("scale", Some(value)) => match value.parse::<f64>() {
                Ok(scale) if scale.is_finite() => transform.scale = scale,
                _ => emit_parse_error(diagnostics, format!("invalid scale `{value}`")),
            },
            ("rotate", Some(value)) => match value.parse::<f64>() {
                Ok(rotate) if rotate.is_finite() => transform.rotate = rotate,
                _ => emit_parse_error(diagnostics, format!("invalid rotate `{value}`")),
            },
            _ => emit_unsupported(diagnostics, option.to_string()),
        }
    }

    transform
}

fn wrap_with_transform(
    node: GraphicNode,
    state: ScopeState,
    transform: Transform2D,
) -> GraphicNode {
    if transform == Transform2D::default() {
        return node;
    }

    GraphicNode::Group(GraphicGroup {
        children: vec![node],
        default_stroke: state.default_stroke,
        default_fill: state.default_fill,
        default_line_width: state.default_line_width,
        clip_path: None,
        transform,
    })
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
        compile_graphics_scene, ArrowSpec, GraphicGroup, GraphicNode, GraphicText, GraphicsScene,
        PathSegment, Point, Transform2D, VectorPrimitive,
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
                arrows: ArrowSpec::None,
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
                arrows: ArrowSpec::None,
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
        assert_eq!(primitive.arrows, ArrowSpec::None);
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
        assert_eq!(primitive.arrows, ArrowSpec::None);
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
    fn parses_clip_command_into_group() {
        let result = parse_tikzpicture(r"\clip (0,0) rectangle (1,1);\draw (0,0) -- (1,0);");

        assert!(result.diagnostics.is_empty());
        assert_eq!(
            result.scene.nodes,
            vec![GraphicNode::Group(GraphicGroup {
                children: vec![GraphicNode::Vector(VectorPrimitive {
                    path: vec![
                        PathSegment::MoveTo(Point { x: 0.0, y: 0.0 }),
                        PathSegment::LineTo(Point {
                            x: CM_IN_PT,
                            y: 0.0,
                        }),
                    ],
                    stroke: Some(named_color("black")),
                    fill: None,
                    line_width: 0.4,
                    arrows: ArrowSpec::None,
                })],
                default_stroke: None,
                default_fill: None,
                default_line_width: Some(0.4),
                clip_path: Some(vec![
                    PathSegment::MoveTo(Point { x: 0.0, y: 0.0 }),
                    PathSegment::LineTo(Point {
                        x: CM_IN_PT,
                        y: 0.0,
                    }),
                    PathSegment::LineTo(Point {
                        x: CM_IN_PT,
                        y: CM_IN_PT,
                    }),
                    PathSegment::LineTo(Point {
                        x: 0.0,
                        y: CM_IN_PT,
                    }),
                    PathSegment::ClosePath,
                ]),
                transform: Transform2D::default(),
            })]
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

    #[test]
    fn parses_scope_style_inheritance_and_override() {
        let result = parse_tikzpicture(
            r"\begin{scope}[draw=red]
                \draw (0,0) -- (1,0);
                \begin{scope}[draw=blue]
                    \draw (0,1) -- (1,1);
                \end{scope}
            \end{scope}",
        );

        assert!(result.diagnostics.is_empty());
        let [GraphicNode::Group(outer)] = result.scene.nodes.as_slice() else {
            panic!("expected outer scope group");
        };
        assert_eq!(outer.default_stroke, Some(named_color("red")));
        let [GraphicNode::Vector(red_line), GraphicNode::Group(inner)] = outer.children.as_slice()
        else {
            panic!("expected outer vector and inner scope");
        };
        assert_eq!(red_line.stroke, Some(named_color("red")));
        assert_eq!(inner.default_stroke, Some(named_color("blue")));
        let [GraphicNode::Vector(blue_line)] = inner.children.as_slice() else {
            panic!("expected inner vector");
        };
        assert_eq!(blue_line.stroke, Some(named_color("blue")));
    }

    #[test]
    fn parses_three_level_nested_scopes() {
        let result = parse_tikzpicture(
            r"\begin{scope}[draw=red]
                \begin{scope}[draw=green]
                    \begin{scope}[draw=blue]
                        \draw (0,0) -- (1,0);
                    \end{scope}
                \end{scope}
            \end{scope}",
        );

        assert!(result.diagnostics.is_empty());
        let [GraphicNode::Group(level1)] = result.scene.nodes.as_slice() else {
            panic!("expected level1 group");
        };
        let [GraphicNode::Group(level2)] = level1.children.as_slice() else {
            panic!("expected level2 group");
        };
        let [GraphicNode::Group(level3)] = level2.children.as_slice() else {
            panic!("expected level3 group");
        };
        let [GraphicNode::Vector(line)] = level3.children.as_slice() else {
            panic!("expected vector node");
        };
        assert_eq!(level1.default_stroke, Some(named_color("red")));
        assert_eq!(level2.default_stroke, Some(named_color("green")));
        assert_eq!(level3.default_stroke, Some(named_color("blue")));
        assert_eq!(line.stroke, Some(named_color("blue")));
    }

    #[test]
    fn parses_scope_and_command_transforms() {
        let result = parse_tikzpicture(
            r"\begin{scope}[shift={(1,2)}]
                \draw[xshift=10pt,yshift=5pt,scale=2,rotate=45] (0,0) -- (1,0);
            \end{scope}",
        );

        assert!(result.diagnostics.is_empty());
        let [GraphicNode::Group(scope)] = result.scene.nodes.as_slice() else {
            panic!("expected scope group");
        };
        assert_eq!(
            scope.transform,
            Transform2D {
                x_shift: CM_IN_PT,
                y_shift: 2.0 * CM_IN_PT,
                scale: 1.0,
                rotate: 0.0,
            }
        );
        let [GraphicNode::Group(local_transform_group)] = scope.children.as_slice() else {
            panic!("expected local transform wrapper");
        };
        assert_eq!(
            local_transform_group.transform,
            Transform2D {
                x_shift: 10.0,
                y_shift: 5.0,
                scale: 2.0,
                rotate: 45.0,
            }
        );
    }

    #[test]
    fn parses_arrow_options() {
        let result = parse_tikzpicture(
            r"\draw[->] (0,0) -- (1,0);
               \draw[<-] (0,1) -- (1,1);
               \draw[<->] (0,2) -- (1,2);",
        );

        assert!(result.diagnostics.is_empty());
        let [GraphicNode::Vector(forward), GraphicNode::Vector(backward), GraphicNode::Vector(both)] =
            result.scene.nodes.as_slice()
        else {
            panic!("expected three vector nodes");
        };
        assert_eq!(forward.arrows, ArrowSpec::Forward);
        assert_eq!(backward.arrows, ArrowSpec::Backward);
        assert_eq!(both.arrows, ArrowSpec::Both);
    }

    #[test]
    fn unsupported_scope_options_emit_diagnostics_but_preserve_supported_ones() {
        let result = parse_tikzpicture(
            r"\begin{scope}[draw=red,foo=bar]
                \draw (0,0) -- (1,0);
            \end{scope}",
        );

        assert_eq!(
            result.diagnostics,
            vec![TikzDiagnostic::UnsupportedCommand {
                command: "foo=bar".to_string(),
            }]
        );
        let [GraphicNode::Group(scope)] = result.scene.nodes.as_slice() else {
            panic!("expected scope group");
        };
        assert_eq!(scope.default_stroke, Some(named_color("red")));
    }

    #[test]
    fn compile_graphics_scene_from_grouped_tikz_result_uses_bounds() {
        let parsed = parse_tikzpicture(
            r"\begin{scope}[xshift=1cm,yshift=2cm]
                \draw (0,0) rectangle (2,1);
            \end{scope}",
        );
        let graphics_box = compile_graphics_scene(parsed.scene);

        assert_eq!(
            graphics_box.width.0,
            (2.0 * CM_IN_PT * 65_536.0).round() as i64
        );
        assert_eq!(graphics_box.height.0, (CM_IN_PT * 65_536.0).round() as i64);
        assert!(matches!(
            graphics_box
                .scene
                .as_ref()
                .map(|scene| scene.nodes.as_slice()),
            Some([GraphicNode::Group(_)])
        ));
    }
}
