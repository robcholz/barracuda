use core::{ffi::CStr, slice::from_raw_parts};

/// Structure for defining what information needs to be extracted from a function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DebugFlags {
    flags: u8,
}

const fn has_bit(flags: u8, mask: u8) -> bool {
    (flags & mask) != 0
}

impl DebugFlags {
    const FLAG_FUNC: u8 = (1 << 0);
    const FLAG_CURRENT_LINE: u8 = (1 << 1);
    const FLAG_NAME: u8 = (1 << 2);
    const FLAG_TRANSFER: u8 = (1 << 3);
    const FLAG_SOURCE: u8 = (1 << 4);
    const FLAG_TAIL_CALL: u8 = (1 << 5);
    const FLAG_PROTO: u8 = (1 << 6);
    const FLAG_LINES: u8 = (1 << 7);

    /// Require no information.
    pub const fn nothing() -> Self {
        Self { flags: 0 }
    }

    /// Require all information
    /// except lines of source code pushed onto the stack.
    pub const fn everything() -> Self {
        macro_rules! union {
			($flag1:ident $($flag:ident)*) => {
				Self::$flag1 $(| Self::$flag)*
			};
		}
        Self {
            flags: union!(FLAG_FUNC FLAG_CURRENT_LINE FLAG_NAME FLAG_TRANSFER FLAG_SOURCE FLAG_TAIL_CALL FLAG_PROTO),
        }
    }

    /// Write out a "what" string into `buffer`
    /// and return the slice that contains it.
    ///
    /// The maximum number of C characters that can be written to `buffer` is
    /// `10`, including the zero-terminator.
    pub const fn write_string<'a>(&self, func_from_stack: bool, buffer: &'a mut [u8]) -> &'a CStr {
        let flags = self.flags;

        let mut i = 0;

        if func_from_stack {
            buffer[i] = b'>';
            i += 1;
        }
        if has_bit(flags, Self::FLAG_FUNC) {
            buffer[i] = b'f';
            i += 1;
        }
        if has_bit(flags, Self::FLAG_CURRENT_LINE) {
            buffer[i] = b'l';
            i += 1;
        }
        if has_bit(flags, Self::FLAG_NAME) {
            buffer[i] = b'n';
            i += 1;
        }
        if has_bit(flags, Self::FLAG_TRANSFER) {
            buffer[i] = b'r';
            i += 1;
        }
        if has_bit(flags, Self::FLAG_SOURCE) {
            buffer[i] = b'S';
            i += 1;
        }
        if has_bit(flags, Self::FLAG_TAIL_CALL) {
            buffer[i] = b't';
            i += 1;
        }
        if has_bit(flags, Self::FLAG_PROTO) {
            buffer[i] = b'u';
            i += 1;
        }
        if has_bit(flags, Self::FLAG_LINES) {
            buffer[i] = b'L';
            i += 1;
        }
        buffer[i] = 0;

        unsafe { CStr::from_bytes_with_nul_unchecked(from_raw_parts(buffer.as_ptr(), i)) }
    }

    /// Allocates a valid buffer for a [`CStr`] with the information selection options
    /// and calls a function with it.
    pub fn with_string<R, F: FnOnce(&CStr) -> R>(&self, using_function: bool, f: F) -> R {
        let mut buffer = [0u8; 10];
        let s = self.write_string(using_function, &mut buffer);
        f(s)
    }

    /// Require function (push function onto the stack).
    pub const fn with_func(self) -> Self {
        Self {
            flags: self.flags | Self::FLAG_FUNC,
        }
    }

    /// Require current line
    /// (`currentline`).
    pub const fn with_current_line(self) -> Self {
        Self {
            flags: self.flags | Self::FLAG_CURRENT_LINE,
        }
    }

    /// Require name information
    /// (`name`, `namewhat`).
    pub const fn with_name(self) -> Self {
        Self {
            flags: self.flags | Self::FLAG_NAME,
        }
    }

    /// Require transfer information
    /// (`ftransfer`, `ntransfer`).
    pub const fn with_transfer_info(self) -> Self {
        Self {
            flags: self.flags | Self::FLAG_TRANSFER,
        }
    }

    /// Require source information
    /// (`source`, `srclen`, `short_src`, `linedefined`, `lastlinedefined`, `what`).
    pub const fn with_source_info(self) -> Self {
        Self {
            flags: self.flags | Self::FLAG_SOURCE,
        }
    }

    /// Require tall call information (`istailcall`).
    pub const fn with_tail_call_info(self) -> Self {
        Self {
            flags: self.flags | Self::FLAG_TAIL_CALL,
        }
    }

    /// Require function prototype information
    /// (`nupvalues`, `nparams`, `isvararg`).
    pub const fn with_prototype(self) -> Self {
        Self {
            flags: self.flags | Self::FLAG_PROTO,
        }
    }

    /// Require source code lines (push table of them onto the stack).
    pub const fn with_lines(self) -> Self {
        Self {
            flags: self.flags | Self::FLAG_LINES,
        }
    }
}
