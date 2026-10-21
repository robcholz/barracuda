//! BVIR: the binary form of a linked [`Program`].
//!
//! The host links View IR once; the device only reads and validates. Every
//! index is checked on load, so a running program never looks up a slot,
//! handler or loop item that does not exist.
//!
//! Layout (little-endian; strings are a `u16` byte length then UTF-8):
//!
//! ```text
//! "BVIR" u16 version
//! u16 struct count, structs: name, u8 field count, fields: name, type
//! u16 slot count, slots: name, type
//! u16 handler count, handlers: name, u8 parameter count, types
//! u16 root node count, nodes
//! ```
//!
//! A type is a tag (`0` bool, `1` i32, `2` f32, `3` f64, `4` string,
//! `5` array + element type, `6` struct + `u16` index of an earlier struct).
//!
//! An element's flags say whether it has a dynamic style (bit 0), is
//! focusable (1), has a press handler (2), static text (3), a static rect
//! (4), which follows the style as four `f32`s (x, y, width, height), or a
//! formula rect (5).

use crate::program::{
    BinaryOp, Branch, Cast, Element, Expr, ForEach, Handler, Layout, MAX_NODES, Node, Program,
    Slot, TextPart,
};
use crate::value::{StructType, Type, Value};
use alloc::{boxed::Box, rc::Rc, string::String, vec::Vec};

/// The file signature.
pub const MAGIC: &[u8; 4] = b"BVIR";
/// The format version this crate reads and writes.
pub const VERSION: u16 = 2;
/// The deepest expression or node nesting a file may contain.
pub const MAX_DEPTH: usize = 48;

/// Why a BVIR file was rejected.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FormatError {
    /// The file does not start with [`MAGIC`].
    Magic,
    /// The file has a different format version.
    Version(u16),
    /// The file ends early.
    Truncated,
    /// The file continues after the program.
    TrailingBytes,
    /// A string is not UTF-8.
    Utf8,
    /// A value is out of range; the text names what.
    Invalid(&'static str),
    /// The program exceeds [`MAX_DEPTH`] or [`MAX_NODES`].
    Limit,
}

// ---- writing -----------------------------------------------------------------

/// Encodes `program`.
#[must_use]
pub fn write(program: &Program) -> Vec<u8> {
    let mut structs: Vec<Rc<StructType>> = Vec::new();
    for ty in program.slots.iter().map(|slot| &slot.ty).chain(
        program
            .handlers
            .iter()
            .flat_map(|handler| handler.params.iter()),
    ) {
        collect_structs(ty, &mut structs);
    }
    let mut writer = Writer {
        out: Vec::new(),
        structs,
    };
    writer.out.extend_from_slice(MAGIC);
    writer.u16(VERSION);
    writer.u16(writer.structs.len() as u16);
    for index in 0..writer.structs.len() {
        let ty = Rc::clone(&writer.structs[index]);
        writer.str(&ty.name);
        writer.out.push(ty.fields.len() as u8);
        for (name, field) in &ty.fields {
            writer.str(name);
            writer.ty(field);
        }
    }
    writer.u16(program.slots.len() as u16);
    for slot in &program.slots {
        writer.str(&slot.name);
        writer.ty(&slot.ty);
    }
    writer.u16(program.handlers.len() as u16);
    for handler in &program.handlers {
        writer.str(&handler.name);
        writer.out.push(handler.params.len() as u8);
        for param in &handler.params {
            writer.ty(param);
        }
    }
    writer.nodes(&program.nodes);
    writer.out
}

fn collect_structs(ty: &Type, out: &mut Vec<Rc<StructType>>) {
    match ty {
        Type::Array(element) => collect_structs(element, out),
        Type::Struct(st) => {
            for (_, field) in &st.fields {
                collect_structs(field, out);
            }
            if !out.iter().any(|known| known == st) {
                out.push(Rc::clone(st));
            }
        }
        _ => {}
    }
}

struct Writer {
    out: Vec<u8>,
    structs: Vec<Rc<StructType>>,
}

impl Writer {
    fn u16(&mut self, value: u16) {
        self.out.extend_from_slice(&value.to_le_bytes());
    }

    fn str(&mut self, value: &str) {
        self.u16(value.len() as u16);
        self.out.extend_from_slice(value.as_bytes());
    }

    fn ty(&mut self, ty: &Type) {
        match ty {
            Type::Bool => self.out.push(0),
            Type::I32 => self.out.push(1),
            Type::F32 => self.out.push(2),
            Type::F64 => self.out.push(3),
            Type::Str => self.out.push(4),
            Type::Array(element) => {
                self.out.push(5);
                self.ty(element);
            }
            Type::Struct(st) => {
                self.out.push(6);
                let index = self
                    .structs
                    .iter()
                    .position(|known| known == st)
                    .unwrap_or(0);
                self.u16(index as u16);
            }
        }
    }

    fn value(&mut self, value: &Value) {
        match value {
            Value::Bool(value) => {
                self.out.push(1);
                self.out.push(u8::from(*value));
            }
            Value::I32(value) => {
                self.out.push(2);
                self.out.extend_from_slice(&value.to_le_bytes());
            }
            Value::F32(value) => {
                self.out.push(3);
                self.out.extend_from_slice(&value.to_le_bytes());
            }
            Value::F64(value) => {
                self.out.push(4);
                self.out.extend_from_slice(&value.to_le_bytes());
            }
            Value::Str(value) => {
                self.out.push(5);
                self.str(value);
            }
            Value::Void | Value::Struct(_) | Value::Array(_) => self.out.push(0),
        }
    }

    fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Literal(value) => {
                self.out.push(0);
                self.value(value);
            }
            Expr::Slot(index) => {
                self.out.push(1);
                self.u16(*index);
            }
            Expr::Local(index) => {
                self.out.push(2);
                self.u16(*index);
            }
            Expr::Field(object, index) => {
                self.out.push(3);
                self.expr(object);
                self.u16(*index);
            }
            Expr::Not(operand) => {
                self.out.push(4);
                self.expr(operand);
            }
            Expr::Neg(operand) => {
                self.out.push(5);
                self.expr(operand);
            }
            Expr::Binary(op, left, right) => {
                self.out.push(6);
                self.out.push(*op as u8);
                self.expr(left);
                self.expr(right);
            }
            Expr::Conditional(condition, consequent, alternate) => {
                self.out.push(7);
                self.expr(condition);
                self.expr(consequent);
                self.expr(alternate);
            }
            Expr::Cast(to, value) => {
                self.out.push(8);
                self.out.push(*to as u8);
                self.expr(value);
            }
        }
    }

    fn nodes(&mut self, nodes: &[Node]) {
        self.u16(nodes.len() as u16);
        for node in nodes {
            self.node(node);
        }
    }

    fn node(&mut self, node: &Node) {
        match node {
            Node::Element(element) => {
                self.out.push(0);
                self.out.push(element.kind);
                self.out.extend_from_slice(&element.style.to_le_bytes());
                let flags = u8::from(element.dynamic_style.is_some())
                    | (u8::from(element.focusable) << 1)
                    | (u8::from(element.press.is_some()) << 2)
                    | (u8::from(element.static_text.is_some()) << 3)
                    | (u8::from(matches!(element.layout, Layout::Static { .. })) << 4)
                    | (u8::from(element.layout == Layout::Formula) << 5);
                self.out.push(flags);
                if let Layout::Static { x, y, w, h } = element.layout {
                    for value in [x, y, w, h] {
                        self.out.extend_from_slice(&value.to_le_bytes());
                    }
                }
                if let Some(expr) = &element.dynamic_style {
                    self.expr(expr);
                }
                self.out.push(element.props.len() as u8);
                for (prop, expr) in &element.props {
                    self.out.push(*prop);
                    self.expr(expr);
                }
                if let Some(text) = &element.static_text {
                    self.str(text);
                }
                self.out.push(element.text.len() as u8);
                for part in &element.text {
                    match part {
                        TextPart::Literal(literal) => {
                            self.out.push(0);
                            self.str(literal);
                        }
                        TextPart::Expr(expr) => {
                            self.out.push(1);
                            self.expr(expr);
                        }
                    }
                }
                if let Some((handler, args)) = &element.press {
                    self.u16(*handler);
                    self.out.push(args.len() as u8);
                    for arg in args {
                        self.expr(arg);
                    }
                }
                self.nodes(&element.children);
            }
            Node::If(branches) => {
                self.out.push(1);
                self.out.push(branches.len() as u8);
                for branch in branches {
                    match &branch.condition {
                        Some(condition) => {
                            self.out.push(1);
                            self.expr(condition);
                        }
                        None => self.out.push(0),
                    }
                    self.nodes(&branch.children);
                }
            }
            Node::For(each) => {
                self.out.push(2);
                self.expr(&each.source);
                self.expr(&each.key);
                self.nodes(&each.children);
            }
        }
    }
}

// ---- reading -----------------------------------------------------------------

/// Decodes and validates a program.
///
/// # Errors
///
/// Returns [`FormatError`] for any malformed, truncated or out-of-range input.
pub fn read(bytes: &[u8]) -> Result<Program, FormatError> {
    let mut reader = Reader {
        bytes,
        pos: 0,
        depth: 0,
        nodes: 0,
        locals: 0,
        slots: 0,
        handlers: Vec::new(),
        structs: Vec::new(),
    };
    if reader.take(4)? != MAGIC {
        return Err(FormatError::Magic);
    }
    let version = reader.u16()?;
    if version != VERSION {
        return Err(FormatError::Version(version));
    }
    for _ in 0..reader.u16()? {
        let name = reader.string()?;
        let mut fields = Vec::new();
        for _ in 0..reader.u8()? {
            let field = reader.string()?;
            fields.push((field, reader.ty()?));
        }
        reader.structs.push(Rc::new(StructType { name, fields }));
    }
    let mut slots = Vec::new();
    for _ in 0..reader.u16()? {
        let name = reader.string()?;
        slots.push(Slot {
            name,
            ty: reader.ty()?,
        });
    }
    reader.slots = slots.len();
    let mut handlers = Vec::new();
    for _ in 0..reader.u16()? {
        let name = reader.string()?;
        let mut params = Vec::new();
        for _ in 0..reader.u8()? {
            params.push(reader.ty()?);
        }
        handlers.push(Handler { name, params });
    }
    reader.handlers = handlers
        .iter()
        .map(|handler| handler.params.len())
        .collect();
    let nodes = reader.nodes()?;
    if reader.pos != bytes.len() {
        return Err(FormatError::TrailingBytes);
    }
    Ok(Program {
        nodes,
        slots,
        handlers,
    })
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    depth: usize,
    nodes: usize,
    locals: usize,
    slots: usize,
    handlers: Vec<usize>,
    structs: Vec<Rc<StructType>>,
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], FormatError> {
        let end = self.pos.checked_add(count).ok_or(FormatError::Truncated)?;
        let slice = self
            .bytes
            .get(self.pos..end)
            .ok_or(FormatError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], FormatError> {
        let mut out = [0; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, FormatError> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, FormatError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn string(&mut self) -> Result<String, FormatError> {
        let len = usize::from(self.u16()?);
        let bytes = self.take(len)?;
        core::str::from_utf8(bytes)
            .map(String::from)
            .map_err(|_| FormatError::Utf8)
    }

    fn enter(&mut self) -> Result<(), FormatError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(FormatError::Limit);
        }
        Ok(())
    }

    fn ty(&mut self) -> Result<Type, FormatError> {
        self.enter()?;
        let ty = match self.u8()? {
            0 => Type::Bool,
            1 => Type::I32,
            2 => Type::F32,
            3 => Type::F64,
            4 => Type::Str,
            5 => Type::Array(Box::new(self.ty()?)),
            6 => {
                let index = usize::from(self.u16()?);
                Type::Struct(Rc::clone(
                    self.structs
                        .get(index)
                        .ok_or(FormatError::Invalid("struct index"))?,
                ))
            }
            _ => return Err(FormatError::Invalid("type tag")),
        };
        self.depth -= 1;
        Ok(ty)
    }

    fn value(&mut self) -> Result<Value, FormatError> {
        Ok(match self.u8()? {
            0 => Value::Void,
            1 => Value::Bool(self.u8()? != 0),
            2 => Value::I32(i32::from_le_bytes(self.array()?)),
            3 => Value::F32(f32::from_le_bytes(self.array()?)),
            4 => Value::F64(f64::from_le_bytes(self.array()?)),
            5 => Value::Str(self.string()?.into()),
            _ => return Err(FormatError::Invalid("literal tag")),
        })
    }

    fn expr(&mut self) -> Result<Expr, FormatError> {
        self.enter()?;
        let expr = match self.u8()? {
            0 => Expr::Literal(self.value()?),
            1 => {
                let index = self.u16()?;
                if usize::from(index) >= self.slots {
                    return Err(FormatError::Invalid("slot index"));
                }
                Expr::Slot(index)
            }
            2 => {
                let index = self.u16()?;
                if usize::from(index) >= self.locals {
                    return Err(FormatError::Invalid("local index"));
                }
                Expr::Local(index)
            }
            3 => {
                let object = self.expr()?;
                Expr::Field(Box::new(object), self.u16()?)
            }
            4 => Expr::Not(Box::new(self.expr()?)),
            5 => Expr::Neg(Box::new(self.expr()?)),
            6 => {
                let op = BinaryOp::from_u8(self.u8()?).ok_or(FormatError::Invalid("operator"))?;
                let left = self.expr()?;
                Expr::Binary(op, Box::new(left), Box::new(self.expr()?))
            }
            7 => {
                let condition = self.expr()?;
                let consequent = self.expr()?;
                Expr::Conditional(
                    Box::new(condition),
                    Box::new(consequent),
                    Box::new(self.expr()?),
                )
            }
            8 => {
                let to = match self.u8()? {
                    0 => Cast::F32,
                    1 => Cast::F64,
                    2 => Cast::I32,
                    _ => return Err(FormatError::Invalid("cast")),
                };
                Expr::Cast(to, Box::new(self.expr()?))
            }
            _ => return Err(FormatError::Invalid("expression tag")),
        };
        self.depth -= 1;
        Ok(expr)
    }

    fn nodes(&mut self) -> Result<Vec<Node>, FormatError> {
        let count = usize::from(self.u16()?);
        let mut nodes = Vec::with_capacity(count.min(MAX_NODES));
        for _ in 0..count {
            nodes.push(self.node()?);
        }
        Ok(nodes)
    }

    fn node(&mut self) -> Result<Node, FormatError> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(FormatError::Limit);
        }
        self.enter()?;
        let node = match self.u8()? {
            0 => Node::Element(self.element()?),
            1 => {
                let mut branches = Vec::new();
                for _ in 0..self.u8()? {
                    let condition = match self.u8()? {
                        0 => None,
                        1 => Some(self.expr()?),
                        _ => return Err(FormatError::Invalid("branch flag")),
                    };
                    branches.push(Branch {
                        condition,
                        children: self.nodes()?,
                    });
                }
                Node::If(branches)
            }
            2 => {
                let source = self.expr()?;
                self.locals += 1;
                let key = self.expr();
                let children = self.nodes();
                self.locals -= 1;
                Node::For(Box::new(ForEach {
                    source,
                    key: key?,
                    children: children?,
                }))
            }
            _ => return Err(FormatError::Invalid("node tag")),
        };
        self.depth -= 1;
        Ok(node)
    }

    fn element(&mut self) -> Result<Element, FormatError> {
        let kind = self.u8()?;
        if kind > 2 {
            return Err(FormatError::Invalid("element kind"));
        }
        let style = i32::from_le_bytes(self.array()?);
        let flags = self.u8()?;
        if flags & !0b11_1111 != 0 || flags & 0b11_0000 == 0b11_0000 {
            return Err(FormatError::Invalid("element flags"));
        }
        let layout = if flags & 0b1_0000 != 0 {
            let mut rect = [0.0; 4];
            for value in &mut rect {
                *value = f32::from_le_bytes(self.array()?);
                if !value.is_finite() {
                    return Err(FormatError::Invalid("element rect"));
                }
            }
            let [x, y, w, h] = rect;
            Layout::Static { x, y, w, h }
        } else if flags & 0b10_0000 != 0 {
            Layout::Formula
        } else {
            Layout::Live
        };
        let dynamic_style = if flags & 1 != 0 {
            Some(self.expr()?)
        } else {
            None
        };
        let mut props = Vec::new();
        for _ in 0..self.u8()? {
            let prop = self.u8()?;
            props.push((prop, self.expr()?));
        }
        let static_text = if flags & 0b1000 != 0 {
            Some(self.string()?)
        } else {
            None
        };
        let mut text = Vec::new();
        for _ in 0..self.u8()? {
            text.push(match self.u8()? {
                0 => TextPart::Literal(self.string()?),
                1 => TextPart::Expr(self.expr()?),
                _ => return Err(FormatError::Invalid("text part")),
            });
        }
        let press = if flags & 0b100 != 0 {
            let handler = self.u16()?;
            let arity = *self
                .handlers
                .get(usize::from(handler))
                .ok_or(FormatError::Invalid("handler index"))?;
            let count = usize::from(self.u8()?);
            if count != arity {
                return Err(FormatError::Invalid("handler arity"));
            }
            let mut args = Vec::with_capacity(count);
            for _ in 0..count {
                args.push(self.expr()?);
            }
            Some((handler, args))
        } else {
            None
        };
        Ok(Element {
            kind,
            style,
            layout,
            dynamic_style,
            props,
            static_text,
            text,
            focusable: flags & 0b10 != 0,
            press,
            children: self.nodes()?,
        })
    }
}
