use core::{ffi::c_int, num::NonZero};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct AbsIndex(NonZero<c_int>);
impl From<NonZero<c_int>> for AbsIndex {
    fn from(value: NonZero<c_int>) -> Self {
        Self(value)
    }
}
impl AbsIndex {
    pub const fn new(index: c_int) -> Option<Self> {
        if index >= 1 {
            unsafe { Some(Self(NonZero::new_unchecked(index))) }
        } else {
            None
        }
    }

    pub const fn new_unchecked(index: c_int) -> Self {
        unsafe { Self(NonZero::new_unchecked(index)) }
    }

    pub const fn get_nonzero(self) -> NonZero<c_int> {
        self.0
    }

    pub const fn get(self) -> c_int {
        self.0.get()
    }
}

pub type ValidIndex = NonZero<c_int>;
pub type AcceptableIndex = c_int;
