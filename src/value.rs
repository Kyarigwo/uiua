use std::{cmp::Ordering, fmt, mem::ManuallyDrop, ops::*};

use crate::{ast::BinOp, lex::Span, UiuaResult};

pub struct Value {
    ty: Type,
    data: ValueData,
}

fn _keep_value_small(_: std::convert::Infallible) {
    let _: [u8; 16] = unsafe { std::mem::transmute(Value::unit()) };
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.ty {
            Type::Unit => write!(f, "unit"),
            Type::Bool => write!(f, "{}", unsafe { self.data.bool }),
            Type::Int => write!(f, "{}", unsafe { self.data.int }),
            Type::Real => write!(f, "{}", unsafe { self.data.real }),
            Type::Function => write!(f, "function"),
            Type::Partial => write!(f, "partial({})", unsafe { self.data.partial.args.len() }),
        }
    }
}

union ValueData {
    unit: (),
    bool: bool,
    int: i64,
    real: f64,
    function: Function,
    partial: ManuallyDrop<Box<Partial>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Type {
    Unit,
    Bool,
    Int,
    Real,
    Function,
    Partial,
}

const TYPE_ARITY: usize = 6;

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Unit => write!(f, "unit"),
            Type::Bool => write!(f, "bool"),
            Type::Int => write!(f, "int"),
            Type::Real => write!(f, "real"),
            Type::Function => write!(f, "function"),
            Type::Partial => write!(f, "partial"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Function(pub(crate) usize);

impl nohash_hasher::IsEnabled for Function {}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Partial {
    pub(crate) func: Function,
    pub(crate) args: Vec<Value>,
}

impl Value {
    pub fn unit() -> Self {
        Self {
            ty: Type::Unit,
            data: ValueData { unit: () },
        }
    }
    pub fn is_unit(&self) -> bool {
        self.ty == Type::Unit
    }
    pub fn ty(&self) -> Type {
        self.ty
    }
    pub fn is_truthy(&self) -> bool {
        !(self.is_unit() || (self.ty == Type::Bool && unsafe { !self.data.bool }))
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Self {
            ty: Type::Bool,
            data: ValueData { bool: b },
        }
    }
}

impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Self {
            ty: Type::Int,
            data: ValueData { int: i },
        }
    }
}

impl From<f64> for Value {
    fn from(r: f64) -> Self {
        Self {
            ty: Type::Real,
            data: ValueData { real: r },
        }
    }
}

impl From<Function> for Value {
    fn from(f: Function) -> Self {
        Self {
            ty: Type::Function,
            data: ValueData { function: f },
        }
    }
}

impl From<Partial> for Value {
    fn from(p: Partial) -> Self {
        Self {
            ty: Type::Partial,
            data: ValueData {
                partial: ManuallyDrop::new(Box::new(p)),
            },
        }
    }
}

impl TryFrom<Value> for bool {
    type Error = Value;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        if value.ty == Type::Bool {
            Ok(unsafe { value.data.bool })
        } else {
            Err(value)
        }
    }
}

impl TryFrom<Value> for i64 {
    type Error = Value;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        if value.ty == Type::Int {
            Ok(unsafe { value.data.int })
        } else {
            Err(value)
        }
    }
}

impl TryFrom<Value> for f64 {
    type Error = Value;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        if value.ty == Type::Real {
            Ok(unsafe { value.data.real })
        } else {
            Err(value)
        }
    }
}

impl TryFrom<Value> for Function {
    type Error = Value;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        if value.ty == Type::Function {
            Ok(unsafe { value.data.function })
        } else {
            Err(value)
        }
    }
}

impl TryFrom<Value> for Partial {
    type Error = Value;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        if value.ty == Type::Partial {
            Ok(unsafe { (**value.data.partial).clone() })
        } else {
            Err(value)
        }
    }
}

static DROP_TABLE: [fn(&mut ValueData); TYPE_ARITY] = [
    |_| {},
    |_| {},
    |_| {},
    |_| {},
    |_| {},
    |data| unsafe {
        ManuallyDrop::drop(&mut data.partial);
    },
];

impl Drop for Value {
    fn drop(&mut self) {
        DROP_TABLE[self.ty as usize](&mut self.data);
    }
}

static CLONE_TABLE: [fn(&ValueData) -> ValueData; TYPE_ARITY] = [
    |_| ValueData { unit: () },
    |data| ValueData {
        bool: unsafe { data.bool },
    },
    |data| ValueData {
        int: unsafe { data.int },
    },
    |data| ValueData {
        real: unsafe { data.real },
    },
    |data| ValueData {
        function: unsafe { data.function },
    },
    |data| ValueData {
        partial: ManuallyDrop::new(unsafe { (*data.partial).clone() }),
    },
];

impl Clone for Value {
    fn clone(&self) -> Self {
        Self {
            ty: self.ty,
            data: CLONE_TABLE[self.ty as usize](&self.data),
        }
    }
}

impl Value {
    pub fn bin_op(&mut self, other: Self, op: BinOp, span: &Span) -> UiuaResult {
        #[cfg(feature = "profile")]
        puffin::profile_function!();
        match op {
            BinOp::Add => self.add_assign(other, span)?,
            BinOp::Sub => self.sub_assign(other, span)?,
            BinOp::Mul => self.mul_assign(other, span)?,
            BinOp::Div => self.div_assign(other, span)?,
            BinOp::Eq => *self = (*self == other).into(),
            BinOp::Ne => *self = (*self != other).into(),
            BinOp::Lt => *self = (*self < other).into(),
            BinOp::Gt => *self = (*self > other).into(),
            BinOp::Le => *self = (*self <= other).into(),
            BinOp::Ge => *self = (*self >= other).into(),
            BinOp::RangeEx => todo!(),
        }
        Ok(())
    }
}

type MathFn = fn(*mut Value, Value, span: &Span) -> UiuaResult;

macro_rules! type_line {
    ($a:expr, $f:expr) => {
        [
            $f($a, Type::Unit),
            $f($a, Type::Bool),
            $f($a, Type::Int),
            $f($a, Type::Real),
            $f($a, Type::Function),
            $f($a, Type::Partial),
        ]
    };
}

macro_rules! type_square {
    ($f:expr) => {
        [
            type_line!(Type::Unit, $f),
            type_line!(Type::Bool, $f),
            type_line!(Type::Int, $f),
            type_line!(Type::Real, $f),
            type_line!(Type::Function, $f),
            type_line!(Type::Partial, $f),
        ]
    };
}

macro_rules! value_bin_op {
    ($table:ident, $fn_name:ident, $method:ident, $verb:literal) => {
        static mut $table: [[MathFn; TYPE_ARITY]; TYPE_ARITY] = type_square!($fn_name);
        const fn $fn_name(a: Type, b: Type) -> MathFn {
            unsafe {
                match (a, b) {
                    (Type::Unit, Type::Unit) => |_, _, _| Ok(()),
                    (Type::Int, Type::Int) => |a, b, _| {
                        (*a).data.int.$method(b.data.int);
                        Ok(())
                    },
                    (Type::Real, Type::Real) => |a, b, _| {
                        (*a).data.real.$method(b.data.real);
                        Ok(())
                    },
                    _ => |a, b, span| {
                        Err(span
                            .clone()
                            .sp(format!("cannot {} {} and {}", $verb, (*a).ty, b.ty))
                            .into())
                    },
                }
            }
        }
        impl Value {
            pub fn $method(&mut self, other: Self, span: &Span) -> UiuaResult {
                #[cfg(feature = "profile")]
                puffin::profile_function!();
                unsafe { $table[self.ty as usize][other.ty as usize](self, other, span) }
            }
        }
    };
}

value_bin_op!(ADD_TABLE, add_fn, add_assign, "add");
value_bin_op!(SUB_TABLE, sub_fn, sub_assign, "subtract");
value_bin_op!(MUL_TABLE, mul_fn, mul_assign, "multiply");
value_bin_op!(DIV_TABLE, div_fn, div_assign, "divide");

type EqFn = unsafe fn(&Value, &Value) -> bool;

static mut EQ_TABLE: [[EqFn; TYPE_ARITY]; TYPE_ARITY] = type_square!(eq_fn);
const fn eq_fn(a: Type, b: Type) -> EqFn {
    unsafe {
        match (a, b) {
            (Type::Unit, Type::Unit) => |_, _| true,
            (Type::Bool, Type::Bool) => |a, b| a.data.bool == b.data.bool,
            (Type::Int, Type::Int) => |a, b| a.data.int == b.data.int,
            (Type::Real, Type::Real) => {
                |a, b| a.data.real.is_nan() && b.data.real.is_nan() || a.data.real == b.data.real
            }
            _ => |_, _| false,
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        #[cfg(feature = "profile")]
        puffin::profile_function!();
        unsafe { EQ_TABLE[self.ty as usize][other.ty as usize](self, other) }
    }
}

impl Eq for Value {}

type CmpFn = fn(&Value, &Value) -> Ordering;

static mut CMP_TABLE: [[CmpFn; TYPE_ARITY]; TYPE_ARITY] = type_square!(cmp_fn);

const fn cmp_fn(a: Type, b: Type) -> CmpFn {
    unsafe {
        match (a, b) {
            (Type::Unit, Type::Unit) => |_, _| Ordering::Equal,
            (Type::Bool, Type::Bool) => |a, b| a.data.bool.cmp(&b.data.bool),
            (Type::Int, Type::Int) => |a, b| a.data.int.cmp(&b.data.int),
            (Type::Real, Type::Real) => |a, b| match (a.data.real.is_nan(), b.data.real.is_nan()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                (false, false) => a.data.real.partial_cmp(&b.data.real).unwrap(),
            },
            _ => |_, _| Ordering::Equal,
        }
    }
}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Value {
    fn cmp(&self, other: &Self) -> Ordering {
        #[cfg(feature = "profile")]
        puffin::profile_function!();
        unsafe { CMP_TABLE[self.ty as usize][other.ty as usize](self, other) }
    }
}
