//! Links MicroTS View IR (the JSON `buildAot` emits) into a [`Program`].
//!
//! Only the first component is linked. Values and handlers keep their IR order;
//! named struct types become [`StructType`]s shared by every use.
//!
//! An element may carry the rect a build solved for it, as the build scripts
//! add it from PocketJS's layout plan: `"layout": {"mode": "static", "rect":
//! [x, y, width, height]}` or `{"mode": "formula"}`.

use crate::program::{
    BinaryOp, Branch, Cast, Element, Expr, ForEach, Handler, Layout, MAX_NODES, Node, Program,
    Slot, TextPart,
};
use crate::value::{StructType, Type, Value};
use alloc::{boxed::Box, rc::Rc, string::String, vec::Vec};
use serde::Deserialize;

/// Why View IR could not be linked.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LinkError {
    /// The input is not View IR JSON.
    Json,
    /// The IR version is not 1.
    Version,
    /// The IR uses a construct compiled views do not support yet; the text names it.
    Unsupported(&'static str),
    /// The IR refers to something it does not declare; the text names it.
    Unknown(&'static str),
    /// The view has more than [`MAX_NODES`] nodes.
    TooManyNodes,
}

#[derive(Deserialize)]
struct IrProgram {
    version: u32,
    types: Vec<IrTypeDecl>,
    components: Vec<IrComponent>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum IrTypeDecl {
    Struct {
        name: String,
        fields: Vec<IrField>,
    },
    Newtype {
        name: String,
        base: IrType,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct IrField {
    name: String,
    #[serde(rename = "type")]
    ty: IrType,
}

#[derive(Deserialize)]
struct IrComponent {
    values: Vec<IrField>,
    functions: Vec<IrFunction>,
    nodes: Vec<IrNode>,
}

#[derive(Deserialize)]
struct IrFunction {
    name: String,
    #[serde(default)]
    parameters: Vec<IrField>,
}

#[derive(Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum IrType {
    Number {
        name: String,
    },
    String,
    Boolean,
    Void,
    Array {
        element: Box<IrType>,
    },
    Named {
        name: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum IrNode {
    Element {
        tag: String,
        style: i32,
        #[serde(rename = "dynamicStyle")]
        dynamic_style: Option<IrMemo>,
        props: Vec<IrProp>,
        text: Option<IrText>,
        focusable: bool,
        events: Vec<IrEvent>,
        children: Vec<IrNode>,
        #[serde(default)]
        layout: Option<IrLayout>,
    },
    If {
        branches: Vec<IrBranch>,
    },
    For {
        source: IrExpr,
        item: String,
        key: IrExpr,
        children: Vec<IrNode>,
    },
}

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
enum IrLayout {
    Static { rect: [f32; 4] },
    Formula,
}

#[derive(Deserialize)]
struct IrMemo {
    expression: IrExpr,
}

#[derive(Deserialize)]
struct IrProp {
    prop: u8,
    value: IrExpr,
}

#[derive(Deserialize)]
struct IrText {
    parts: Vec<IrPart>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum IrPart {
    Literal(String),
    Expr(IrExpr),
}

#[derive(Deserialize)]
struct IrEvent {
    name: String,
    handler: IrHandler,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum IrHandler {
    Call { expression: IrExpr },
}

#[derive(Deserialize)]
struct IrBranch {
    condition: Option<IrExpr>,
    children: Vec<IrNode>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum IrExpr {
    Literal {
        value: serde_json::Value,
        #[serde(rename = "type")]
        ty: IrType,
    },
    Binding {
        name: String,
        scope: String,
        #[serde(rename = "type")]
        ty: IrType,
    },
    Field {
        object: Box<IrExpr>,
        name: String,
        #[serde(rename = "type")]
        ty: IrType,
    },
    Unary {
        operator: String,
        operand: Box<IrExpr>,
    },
    Binary {
        operator: String,
        left: Box<IrExpr>,
        right: Box<IrExpr>,
    },
    Conditional {
        condition: Box<IrExpr>,
        consequent: Box<IrExpr>,
        alternate: Box<IrExpr>,
    },
    Call {
        target: String,
        name: String,
        arguments: Vec<IrExpr>,
    },
    Cast {
        value: Box<IrExpr>,
        #[serde(rename = "type")]
        ty: IrType,
    },
}

/// Links the first component of `json` into a program.
///
/// # Errors
///
/// Returns [`LinkError`] when the IR is malformed or uses an unsupported construct.
pub fn link(json: &[u8]) -> Result<Program, LinkError> {
    let ir: IrProgram = serde_json::from_slice(json).map_err(|_| LinkError::Json)?;
    if ir.version != 1 {
        return Err(LinkError::Version);
    }
    let component = ir
        .components
        .first()
        .ok_or(LinkError::Unknown("component"))?;
    let mut linker = Linker {
        decls: &ir.types,
        structs: Vec::new(),
        values: &component.values,
        functions: &component.functions,
        locals: Vec::new(),
        nodes: 0,
    };
    let slots = component
        .values
        .iter()
        .map(|value| {
            Ok(Slot {
                name: value.name.clone(),
                ty: linker.ty(&value.ty)?,
            })
        })
        .collect::<Result<Vec<_>, LinkError>>()?;
    let handlers = component
        .functions
        .iter()
        .map(|function| {
            let params = function
                .parameters
                .iter()
                .map(|param| linker.ty(&param.ty))
                .collect::<Result<_, _>>()?;
            Ok(Handler {
                name: function.name.clone(),
                params,
            })
        })
        .collect::<Result<Vec<_>, LinkError>>()?;
    let nodes = linker.nodes(&component.nodes)?;
    Ok(Program {
        nodes,
        slots,
        handlers,
    })
}

struct Linker<'a> {
    decls: &'a [IrTypeDecl],
    structs: Vec<Rc<StructType>>,
    values: &'a [IrField],
    functions: &'a [IrFunction],
    locals: Vec<(&'a str, Option<Type>)>,
    nodes: usize,
}

impl<'a> Linker<'a> {
    fn ty(&mut self, ty: &IrType) -> Result<Type, LinkError> {
        Ok(match ty {
            IrType::Number { name } => match name.as_str() {
                "i32" => Type::I32,
                "f32" => Type::F32,
                _ => Type::F64,
            },
            IrType::String => Type::Str,
            IrType::Boolean => Type::Bool,
            IrType::Array { element } => Type::Array(Box::new(self.ty(element)?)),
            IrType::Named { name } => self.named(name)?,
            IrType::Void | IrType::Other => return Err(LinkError::Unsupported("type")),
        })
    }

    fn named(&mut self, name: &str) -> Result<Type, LinkError> {
        if let Some(known) = self.structs.iter().find(|st| st.name == name) {
            return Ok(Type::Struct(Rc::clone(known)));
        }
        let decls = self.decls;
        for decl in decls {
            match decl {
                IrTypeDecl::Struct {
                    name: decl_name,
                    fields,
                } if decl_name == name => {
                    let fields = fields
                        .iter()
                        .map(|field| Ok((field.name.clone(), self.ty(&field.ty)?)))
                        .collect::<Result<_, LinkError>>()?;
                    let st = Rc::new(StructType {
                        name: String::from(name),
                        fields,
                    });
                    self.structs.push(Rc::clone(&st));
                    return Ok(Type::Struct(st));
                }
                IrTypeDecl::Newtype {
                    name: decl_name,
                    base,
                } if decl_name == name => return self.ty(base),
                _ => {}
            }
        }
        Err(LinkError::Unknown("named type"))
    }

    /// The static type of `expr`, when the IR states one.
    fn static_type(&mut self, expr: &IrExpr) -> Result<Option<Type>, LinkError> {
        Ok(match expr {
            IrExpr::Binding { scope, name, ty } if scope == "local" => {
                match self.locals.iter().rev().find(|(local, _)| local == name) {
                    Some((_, Some(ty))) => Some(ty.clone()),
                    _ => Some(self.ty(ty)?),
                }
            }
            IrExpr::Literal { ty, .. }
            | IrExpr::Binding { ty, .. }
            | IrExpr::Field { ty, .. }
            | IrExpr::Cast { ty, .. } => Some(self.ty(ty)?),
            IrExpr::Conditional { consequent, .. } => self.static_type(consequent)?,
            _ => None,
        })
    }

    fn expr(&mut self, expr: &'a IrExpr) -> Result<Expr, LinkError> {
        Ok(match expr {
            IrExpr::Literal { value, ty } => Expr::Literal(match (value, ty) {
                (serde_json::Value::Bool(value), _) => Value::Bool(*value),
                (serde_json::Value::String(value), _) => Value::Str(Rc::from(value.as_str())),
                (serde_json::Value::Number(number), IrType::Number { name }) if name == "f32" => {
                    Value::F32(
                        number
                            .as_f64()
                            .ok_or(LinkError::Unsupported("number literal"))?
                            as f32,
                    )
                }
                (serde_json::Value::Number(number), IrType::Number { name }) if name != "i32" => {
                    Value::F64(
                        number
                            .as_f64()
                            .ok_or(LinkError::Unsupported("number literal"))?,
                    )
                }
                (serde_json::Value::Number(number), _) => {
                    let int = number
                        .as_i64()
                        .ok_or(LinkError::Unsupported("number literal"))?;
                    Value::I32(
                        i32::try_from(int).map_err(|_| LinkError::Unsupported("number literal"))?,
                    )
                }
                _ => return Err(LinkError::Unsupported("literal")),
            }),
            IrExpr::Binding { name, scope, .. } => match scope.as_str() {
                "vm" => Expr::Slot(index(
                    self.values.iter().position(|value| &value.name == name),
                    "value binding",
                )?),
                "local" => Expr::Local(index(
                    self.locals
                        .iter()
                        .rposition(|(local, _)| *local == name.as_str()),
                    "local binding",
                )?),
                _ => return Err(LinkError::Unsupported("binding scope")),
            },
            IrExpr::Field { object, name, .. } => {
                let Some(Type::Struct(st)) = self.static_type(object)? else {
                    return Err(LinkError::Unsupported("field of a non-struct"));
                };
                let position = st.fields.iter().position(|(field, _)| field == name);
                Expr::Field(Box::new(self.expr(object)?), index(position, "field")?)
            }
            IrExpr::Unary { operator, operand } => match operator.as_str() {
                "!" => Expr::Not(Box::new(self.expr(operand)?)),
                "-" => Expr::Neg(Box::new(self.expr(operand)?)),
                _ => return Err(LinkError::Unsupported("unary operator")),
            },
            IrExpr::Binary {
                operator,
                left,
                right,
            } => {
                let op = match operator.as_str() {
                    "+" => BinaryOp::Add,
                    "-" => BinaryOp::Sub,
                    "*" => BinaryOp::Mul,
                    "<" => BinaryOp::Lt,
                    "<=" => BinaryOp::Le,
                    ">" => BinaryOp::Gt,
                    ">=" => BinaryOp::Ge,
                    "===" | "==" => BinaryOp::Eq,
                    "!==" | "!=" => BinaryOp::Ne,
                    "&&" => BinaryOp::And,
                    "||" => BinaryOp::Or,
                    _ => return Err(LinkError::Unsupported("binary operator")),
                };
                Expr::Binary(op, Box::new(self.expr(left)?), Box::new(self.expr(right)?))
            }
            IrExpr::Conditional {
                condition,
                consequent,
                alternate,
            } => Expr::Conditional(
                Box::new(self.expr(condition)?),
                Box::new(self.expr(consequent)?),
                Box::new(self.expr(alternate)?),
            ),
            IrExpr::Cast { value, ty } => {
                let to = match self.ty(ty)? {
                    Type::F32 => Cast::F32,
                    Type::F64 => Cast::F64,
                    Type::I32 => Cast::I32,
                    _ => return Err(LinkError::Unsupported("cast")),
                };
                Expr::Cast(to, Box::new(self.expr(value)?))
            }
            IrExpr::Call { .. } => return Err(LinkError::Unsupported("call in value position")),
        })
    }

    fn nodes(&mut self, nodes: &'a [IrNode]) -> Result<Vec<Node>, LinkError> {
        nodes.iter().map(|node| self.node(node)).collect()
    }

    fn node(&mut self, node: &'a IrNode) -> Result<Node, LinkError> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(LinkError::TooManyNodes);
        }
        Ok(match node {
            IrNode::Element {
                tag,
                style,
                dynamic_style,
                props,
                text,
                focusable,
                events,
                children,
                layout,
            } => {
                let kind = match tag.as_str() {
                    "View" => 0,
                    "Text" => 1,
                    "Image" => 2,
                    _ => return Err(LinkError::Unsupported("element tag")),
                };
                let (static_text, text) = self.text(text.as_ref())?;
                let mut press = None;
                for event in events {
                    if event.name != "press" {
                        return Err(LinkError::Unsupported("event"));
                    }
                    let IrHandler::Call { expression } = &event.handler;
                    let IrExpr::Call {
                        target,
                        name,
                        arguments,
                    } = expression
                    else {
                        return Err(LinkError::Unsupported("handler expression"));
                    };
                    if target != "vm" {
                        return Err(LinkError::Unsupported("handler target"));
                    }
                    let handler = index(
                        self.functions
                            .iter()
                            .position(|function| &function.name == name),
                        "handler",
                    )?;
                    let args = arguments
                        .iter()
                        .map(|arg| self.expr(arg))
                        .collect::<Result<_, _>>()?;
                    press = Some((handler, args));
                }
                let layout = match layout {
                    None => Layout::Live,
                    Some(IrLayout::Static { rect }) => {
                        if !rect.iter().all(|value| value.is_finite()) {
                            return Err(LinkError::Unsupported("element rect"));
                        }
                        let [x, y, w, h] = *rect;
                        Layout::Static { x, y, w, h }
                    }
                    Some(IrLayout::Formula) => Layout::Formula,
                };
                Node::Element(Element {
                    kind,
                    style: *style,
                    layout,
                    dynamic_style: dynamic_style
                        .as_ref()
                        .map(|memo| self.expr(&memo.expression))
                        .transpose()?,
                    props: props
                        .iter()
                        .map(|prop| Ok((prop.prop, self.expr(&prop.value)?)))
                        .collect::<Result<_, LinkError>>()?,
                    static_text,
                    text,
                    focusable: *focusable,
                    press,
                    children: self.nodes(children)?,
                })
            }
            IrNode::If { branches } => Node::If(
                branches
                    .iter()
                    .map(|branch| {
                        Ok(Branch {
                            condition: branch
                                .condition
                                .as_ref()
                                .map(|c| self.expr(c))
                                .transpose()?,
                            children: self.nodes(&branch.children)?,
                        })
                    })
                    .collect::<Result<_, LinkError>>()?,
            ),
            IrNode::For {
                source,
                item,
                key,
                children,
            } => {
                let element = match self.static_type(source)? {
                    Some(Type::Array(element)) => Some(*element),
                    _ => None,
                };
                let source = self.expr(source)?;
                self.locals.push((item.as_str(), element));
                let key = self.expr(key);
                let children = self.nodes(children);
                self.locals.pop();
                Node::For(Box::new(ForEach {
                    source,
                    key: key?,
                    children: children?,
                }))
            }
        })
    }

    fn text(
        &mut self,
        text: Option<&'a IrText>,
    ) -> Result<(Option<String>, Vec<TextPart>), LinkError> {
        let Some(text) = text else {
            return Ok((None, Vec::new()));
        };
        if text
            .parts
            .iter()
            .all(|part| matches!(part, IrPart::Literal(_)))
        {
            let mut joined = String::new();
            for part in &text.parts {
                if let IrPart::Literal(literal) = part {
                    joined.push_str(literal);
                }
            }
            return Ok((Some(joined), Vec::new()));
        }
        let parts = text
            .parts
            .iter()
            .map(|part| {
                Ok(match part {
                    IrPart::Literal(literal) => TextPart::Literal(literal.clone()),
                    IrPart::Expr(expr) => TextPart::Expr(self.expr(expr)?),
                })
            })
            .collect::<Result<_, LinkError>>()?;
        Ok((None, parts))
    }
}

fn index(position: Option<usize>, what: &'static str) -> Result<u16, LinkError> {
    position
        .and_then(|position| u16::try_from(position).ok())
        .ok_or(LinkError::Unknown(what))
}
