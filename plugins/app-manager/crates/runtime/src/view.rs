//! Mounts a [`Program`] into a PocketJS tree and keeps it in sync with values.
//!
//! The walk mirrors what MicroTS-generated Rust does: siblings update in reverse
//! so each knows its insertion anchor, element bindings are memoised so an
//! unchanged value never touches the tree, and keyed lists reuse rows.

use crate::program::{BinaryOp, Cast, Element, Expr, Layout, Node, Program, TextPart};
use crate::value::{Key, Value};
use alloc::{string::String, vec::Vec};
use microts::{Block, Input, KeyedList, NodeId, StyleId, Ui};

pub(crate) fn eval(expr: &Expr, slots: &[Value], locals: &[Value]) -> Value {
    match expr {
        Expr::Literal(value) => value.clone(),
        Expr::Slot(index) => slots
            .get(usize::from(*index))
            .cloned()
            .unwrap_or(Value::Void),
        Expr::Local(index) => locals
            .get(usize::from(*index))
            .cloned()
            .unwrap_or(Value::Void),
        Expr::Field(object, index) => match eval(object, slots, locals) {
            Value::Struct(fields) => fields
                .get(usize::from(*index))
                .cloned()
                .unwrap_or(Value::Void),
            _ => Value::Void,
        },
        Expr::Not(operand) => Value::Bool(!eval(operand, slots, locals).truthy()),
        Expr::Neg(operand) => match eval(operand, slots, locals) {
            Value::I32(value) => Value::I32(value.wrapping_neg()),
            Value::F32(value) => Value::F32(-value),
            Value::F64(value) => Value::F64(-value),
            other => other,
        },
        Expr::Conditional(condition, consequent, alternate) => {
            if eval(condition, slots, locals).truthy() {
                eval(consequent, slots, locals)
            } else {
                eval(alternate, slots, locals)
            }
        }
        Expr::Cast(to, value) => {
            let value = eval(value, slots, locals);
            match to {
                Cast::F32 => Value::F32(match value {
                    Value::I32(int) => int as f32,
                    other => other.as_f64() as f32,
                }),
                Cast::F64 => Value::F64(value.as_f64()),
                Cast::I32 => Value::I32(value.as_f64() as i32),
            }
        }
        Expr::Binary(op, left, right) => binary(*op, left, right, slots, locals),
    }
}

fn binary(op: BinaryOp, left: &Expr, right: &Expr, slots: &[Value], locals: &[Value]) -> Value {
    if op == BinaryOp::And {
        let left = eval(left, slots, locals);
        return if left.truthy() {
            eval(right, slots, locals)
        } else {
            left
        };
    }
    if op == BinaryOp::Or {
        let left = eval(left, slots, locals);
        return if left.truthy() {
            left
        } else {
            eval(right, slots, locals)
        };
    }
    let (left, right) = (eval(left, slots, locals), eval(right, slots, locals));
    match (op, &left, &right) {
        (BinaryOp::Eq, _, _) => Value::Bool(left == right),
        (BinaryOp::Ne, _, _) => Value::Bool(left != right),
        (_, Value::I32(a), Value::I32(b)) => match op {
            BinaryOp::Add => Value::I32(a.wrapping_add(*b)),
            BinaryOp::Sub => Value::I32(a.wrapping_sub(*b)),
            BinaryOp::Mul => Value::I32(a.wrapping_mul(*b)),
            _ => compare(op, a, b),
        },
        (_, Value::F32(a), Value::F32(b)) => match op {
            BinaryOp::Add => Value::F32(a + b),
            BinaryOp::Sub => Value::F32(a - b),
            BinaryOp::Mul => Value::F32(a * b),
            _ => compare(op, a, b),
        },
        (_, Value::F64(a), Value::F64(b)) => match op {
            BinaryOp::Add => Value::F64(a + b),
            BinaryOp::Sub => Value::F64(a - b),
            BinaryOp::Mul => Value::F64(a * b),
            _ => compare(op, a, b),
        },
        (BinaryOp::Add, Value::Str(a), Value::Str(b)) => {
            let mut joined = String::with_capacity(a.len() + b.len());
            joined.push_str(a);
            joined.push_str(b);
            Value::Str(joined.into())
        }
        _ => Value::Void,
    }
}

fn compare<T: PartialOrd>(op: BinaryOp, a: &T, b: &T) -> Value {
    match op {
        BinaryOp::Lt => Value::Bool(a < b),
        BinaryOp::Le => Value::Bool(a <= b),
        BinaryOp::Gt => Value::Bool(a > b),
        BinaryOp::Ge => Value::Bool(a >= b),
        _ => Value::Void,
    }
}

/// The mounted state of one program node.
enum Instance {
    Element {
        node: NodeId,
        pressable: bool,
        style: Option<i32>,
        props: Vec<Option<Value>>,
        text_inputs: Option<Vec<Value>>,
        text: String,
        children: Instances,
    },
    If {
        selected: Option<usize>,
        block: Instances,
    },
    For {
        rows: KeyedList<Key, Instances>,
    },
}

struct Instances(Vec<Instance>);

impl Instance {
    fn first_node(&self) -> NodeId {
        match self {
            Instance::Element { node, .. } => *node,
            Instance::If { block, .. } => block.first_node(),
            Instance::For { rows } => rows.first_node(),
        }
    }

    fn move_before(&mut self, ui: &mut Ui, parent: NodeId, anchor: NodeId) {
        match self {
            Instance::Element { node, .. } => ui.insert_before(parent, *node, anchor),
            Instance::If { block, .. } => block.move_before(ui, parent, anchor),
            Instance::For { rows } => rows.move_before(ui, parent, anchor),
        }
    }

    fn unmount(self, ui: &mut Ui) {
        match self {
            Instance::Element { node, children, .. } => {
                children.unmount(ui);
                ui.destroy_node(node);
            }
            Instance::If { block, .. } => block.unmount(ui),
            Instance::For { rows } => rows.unmount(ui),
        }
    }
}

impl Block for Instances {
    fn first_node(&self) -> NodeId {
        self.0
            .iter()
            .map(Instance::first_node)
            .find(|node| *node != NodeId::NONE)
            .unwrap_or(NodeId::NONE)
    }

    fn move_before(&mut self, ui: &mut Ui, parent: NodeId, anchor: NodeId) {
        for instance in &mut self.0 {
            instance.move_before(ui, parent, anchor);
        }
    }

    fn unmount(self, ui: &mut Ui) {
        for instance in self.0 {
            instance.unmount(ui);
        }
    }
}

fn mount_nodes(ui: &mut Ui, nodes: &[Node], parent: NodeId, anchor: NodeId) -> Instances {
    Instances(
        nodes
            .iter()
            .map(|node| mount_node(ui, node, parent, anchor))
            .collect(),
    )
}

fn mount_node(ui: &mut Ui, node: &Node, parent: NodeId, anchor: NodeId) -> Instance {
    match node {
        Node::Element(element) => {
            let id = ui.create_node(element.kind);
            // The layout mode comes before the insert, as in PocketJS's
            // generated views: a static node then joins no solver projection.
            match element.layout {
                Layout::Live => {}
                Layout::Static { x, y, w, h } => ui.set_layout_static(id, x, y, w, h),
                Layout::Formula => ui.set_layout_formula(id),
            }
            ui.insert_before(parent, id, anchor);
            ui.set_style(id, StyleId(element.style));
            if element.focusable {
                ui.set_focusable(id, true);
            }
            let children = mount_nodes(ui, &element.children, id, NodeId::NONE);
            if let Some(text) = &element.static_text {
                ui.set_text(id, text);
            }
            Instance::Element {
                node: id,
                pressable: element.focusable,
                style: None,
                props: element.props.iter().map(|_| None).collect(),
                text_inputs: None,
                text: String::new(),
                children,
            }
        }
        Node::If(_) => Instance::If {
            selected: None,
            block: Instances(Vec::new()),
        },
        Node::For(_) => Instance::For {
            rows: KeyedList::new(),
        },
    }
}

/// Reusable buffers for text formatting.
#[derive(Default)]
struct Scratch {
    inputs: Vec<Value>,
    text: String,
}

struct Walk<'a> {
    slots: &'a [Value],
    locals: Vec<Value>,
    scratch: Scratch,
}

impl Walk<'_> {
    fn update_nodes(
        &mut self,
        ui: &mut Ui,
        nodes: &[Node],
        instances: &mut Instances,
        parent: NodeId,
        anchor: NodeId,
    ) {
        let mut next = anchor;
        for (node, instance) in nodes.iter().zip(instances.0.iter_mut()).rev() {
            self.update_node(ui, node, instance, parent, next);
            let first = instance.first_node();
            if first != NodeId::NONE {
                next = first;
            }
        }
    }

    fn update_node(
        &mut self,
        ui: &mut Ui,
        node: &Node,
        instance: &mut Instance,
        parent: NodeId,
        anchor: NodeId,
    ) {
        match (node, instance) {
            (
                Node::Element(element),
                Instance::Element {
                    node,
                    style,
                    props,
                    text_inputs,
                    text,
                    children,
                    ..
                },
            ) => {
                self.update_element(ui, element, *node, style, props, text_inputs, text);
                self.update_nodes(ui, &element.children, children, *node, NodeId::NONE);
            }
            (Node::If(branches), Instance::If { selected, block }) => {
                let want = branches.iter().position(|branch| {
                    branch
                        .condition
                        .as_ref()
                        .is_none_or(|condition| eval(condition, self.slots, &self.locals).truthy())
                });
                if want != *selected {
                    core::mem::replace(block, Instances(Vec::new())).unmount(ui);
                    if let Some(branch) = want.and_then(|index| branches.get(index)) {
                        *block = mount_nodes(ui, &branch.children, parent, anchor);
                    }
                    *selected = want;
                }
                if let Some(branch) = want.and_then(|index| branches.get(index)) {
                    self.update_nodes(ui, &branch.children, block, parent, anchor);
                }
            }
            (Node::For(each), Instance::For { rows }) => {
                let Value::Array(items) = eval(&each.source, self.slots, &self.locals) else {
                    return;
                };
                let mut keys = Vec::with_capacity(items.len());
                for item in items.iter() {
                    self.locals.push(item.clone());
                    keys.push(Key::from(&eval(&each.key, self.slots, &self.locals)));
                    self.locals.pop();
                }
                let key_at = |index: i32| keys.get(index as usize).cloned().unwrap_or(Key::Other);
                rows.reconcile(
                    ui,
                    parent,
                    anchor,
                    &items,
                    |old, _, index| *old == key_at(index),
                    |_, index| key_at(index),
                    |ui, parent, anchor, _, _| mount_nodes(ui, &each.children, parent, anchor),
                    |block, ui, item, _, row_anchor| {
                        self.locals.push(item.clone());
                        self.update_nodes(ui, &each.children, block, parent, row_anchor);
                        self.locals.pop();
                    },
                );
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn update_element(
        &mut self,
        ui: &mut Ui,
        element: &Element,
        node: NodeId,
        style: &mut Option<i32>,
        props: &mut [Option<Value>],
        text_inputs: &mut Option<Vec<Value>>,
        text: &mut String,
    ) {
        if let Some(expr) = &element.dynamic_style
            && let Value::I32(id) = eval(expr, self.slots, &self.locals)
            && *style != Some(id)
        {
            ui.set_style(node, StyleId(id));
            *style = Some(id);
        }
        for ((prop, expr), memo) in element.props.iter().zip(props.iter_mut()) {
            let value = eval(expr, self.slots, &self.locals);
            if memo.as_ref() != Some(&value) {
                ui.set_prop(node, *prop, value.as_f64());
                *memo = Some(value);
            }
        }
        if element.text.is_empty() {
            return;
        }
        let Scratch {
            inputs,
            text: scratch,
        } = &mut self.scratch;
        inputs.clear();
        for part in &element.text {
            if let TextPart::Expr(expr) = part {
                inputs.push(eval(expr, self.slots, &self.locals));
            }
        }
        if text_inputs.as_ref() == Some(inputs) {
            return;
        }
        scratch.clear();
        let mut values = inputs.iter();
        for part in &element.text {
            match part {
                TextPart::Literal(literal) => scratch.push_str(literal),
                TextPart::Expr(_) => {
                    if let Some(value) = values.next() {
                        value.format_into(scratch);
                    }
                }
            }
        }
        if *scratch != *text {
            core::mem::swap(scratch, text);
            ui.set_text(node, text);
        }
        match text_inputs {
            Some(previous) => {
                previous.clear();
                previous.append(inputs);
            }
            None => *text_inputs = Some(core::mem::take(inputs)),
        }
    }

    fn dispatch(
        &mut self,
        nodes: &[Node],
        instances: &Instances,
        input: &Input,
    ) -> Option<(u16, Vec<Value>)> {
        for (node, instance) in nodes.iter().zip(instances.0.iter()) {
            let hit = match (node, instance) {
                (Node::Element(element), Instance::Element { node, children, .. }) => {
                    if let Some((handler, args)) = &element.press
                        && input.is_press(*node)
                    {
                        let args = args
                            .iter()
                            .map(|arg| eval(arg, self.slots, &self.locals))
                            .collect();
                        return Some((*handler, args));
                    }
                    self.dispatch(&element.children, children, input)
                }
                (
                    Node::If(branches),
                    Instance::If {
                        selected: Some(index),
                        block,
                    },
                ) => branches
                    .get(*index)
                    .and_then(|branch| self.dispatch(&branch.children, block, input)),
                (Node::For(each), Instance::For { rows }) => self.dispatch_rows(each, rows, input),
                _ => None,
            };
            if hit.is_some() {
                return hit;
            }
        }
        None
    }

    fn dispatch_rows(
        &mut self,
        each: &crate::program::ForEach,
        rows: &KeyedList<Key, Instances>,
        input: &Input,
    ) -> Option<(u16, Vec<Value>)> {
        let Value::Array(items) = eval(&each.source, self.slots, &self.locals) else {
            return None;
        };
        for row in &rows.rows {
            let item = items.iter().find(|item| {
                self.locals.push((*item).clone());
                let key = Key::from(&eval(&each.key, self.slots, &self.locals));
                self.locals.pop();
                key == row.key
            });
            let Some(item) = item else { continue };
            self.locals.push(item.clone());
            let hit = self.dispatch(&each.children, &row.block, input);
            self.locals.pop();
            if hit.is_some() {
                return hit;
            }
        }
        None
    }
}

/// A program mounted into a [`Ui`].
pub(crate) struct View {
    root: Instances,
    scratch: Scratch,
}

impl View {
    pub(crate) fn mount(ui: &mut Ui, program: &Program) -> Self {
        Self {
            root: mount_nodes(ui, &program.nodes, NodeId::ROOT, NodeId::NONE),
            scratch: Scratch::default(),
        }
    }

    pub(crate) fn update(&mut self, ui: &mut Ui, program: &Program, slots: &[Value]) {
        let mut walk = Walk {
            slots,
            locals: Vec::new(),
            scratch: core::mem::take(&mut self.scratch),
        };
        walk.update_nodes(
            ui,
            &program.nodes,
            &mut self.root,
            NodeId::ROOT,
            NodeId::NONE,
        );
        self.scratch = walk.scratch;
    }

    /// Whether `node` is a mounted element that can receive presses.
    pub(crate) fn is_pressable(&self, node: NodeId) -> bool {
        fn find(instances: &Instances, node: NodeId) -> bool {
            instances.0.iter().any(|instance| match instance {
                Instance::Element {
                    node: id,
                    pressable,
                    children,
                    ..
                } => (*id == node && *pressable) || find(children, node),
                Instance::If { block, .. } => find(block, node),
                Instance::For { rows } => rows.rows.iter().any(|row| find(&row.block, node)),
            })
        }
        find(&self.root, node)
    }

    /// Appends the text of every text-bearing element in document order.
    pub(crate) fn collect_texts(&self, ui: &Ui, out: &mut Vec<String>) {
        fn walk(instances: &Instances, ui: &Ui, out: &mut Vec<String>) {
            for instance in &instances.0 {
                match instance {
                    Instance::Element { node, children, .. } => {
                        if let Some(text) =
                            ui.core().node_text(node.0).filter(|text| !text.is_empty())
                        {
                            out.push(String::from(text));
                        }
                        walk(children, ui, out);
                    }
                    Instance::If { block, .. } => walk(block, ui, out),
                    Instance::For { rows } => {
                        rows.rows.iter().for_each(|row| walk(&row.block, ui, out))
                    }
                }
            }
        }
        walk(&self.root, ui, out);
    }

    /// The handler a resolved press selects, with its evaluated arguments.
    pub(crate) fn dispatch(
        &self,
        program: &Program,
        input: &Input,
        slots: &[Value],
    ) -> Option<(u16, Vec<Value>)> {
        if input.target == NodeId::NONE {
            return None;
        }
        let mut walk = Walk {
            slots,
            locals: Vec::new(),
            scratch: Scratch::default(),
        };
        walk.dispatch(&program.nodes, &self.root, input)
    }
}
