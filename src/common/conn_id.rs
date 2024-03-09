use std::fmt::{Debug, Display};
use std::hash::Hash;
use std::ops::AddAssign;

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RemoteConnId(u32);

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalConnId(u32);

impl LocalConnId {
    pub const fn from_u32(data: u32) -> Self {
        Self(data)
    }
}

pub trait ConnIdTrait:
    Copy + Clone + Debug + Display + Hash + PartialEq + Eq + PartialOrd + Ord + Default + Into<u32>
{
}

pub trait ConnIdProvider<T: ConnIdTrait> {
    fn get_next_id(&mut self) -> T;
    fn is_valid_id(&self, id: &T) -> bool;
}

macro_rules! impl_tuple_from_primitive {
    ($primitive_ty:ident, $tuple_ty:ident) => {
        impl From<$primitive_ty> for $tuple_ty {
            fn from(value: $primitive_ty) -> Self {
                Self(value)
            }
        }
    };
}

macro_rules! impl_primitive_from_tupe {
    ($primitive_ty:ident, $tuple_ty:ident) => {
        impl From<$tuple_ty> for $primitive_ty {
            fn from(value: $tuple_ty) -> Self {
                value.0
            }
        }
    };
}

macro_rules! impl_tuple_add_assign_primitive {
    ($primitive_ty:ident, $tuple_ty:ident) => {
        impl AddAssign<$primitive_ty> for $tuple_ty {
            fn add_assign(&mut self, rhs: $primitive_ty) {
                self.0 += rhs
            }
        }
    };
}

impl_tuple_from_primitive!(u32, RemoteConnId);

impl_primitive_from_tupe!(u32, RemoteConnId);

impl_tuple_add_assign_primitive!(u32, RemoteConnId);

impl_tuple_from_primitive!(u32, LocalConnId);

impl_primitive_from_tupe!(u32, LocalConnId);

impl_tuple_add_assign_primitive!(u32, LocalConnId);

impl Display for RemoteConnId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RemoteConnId({})", self.0)
    }
}

impl Display for LocalConnId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LocalConnId({})", self.0)
    }
}

impl ConnIdTrait for RemoteConnId {}
impl ConnIdTrait for LocalConnId {}

#[derive(Debug, Clone, Copy, Hash)]
pub struct ConnId {
    pub local_id: LocalConnId,
    pub remote_id: RemoteConnId,
}

impl Display for ConnId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`L-{}:R-{}`", self.local_id, self.remote_id)
    }
}
