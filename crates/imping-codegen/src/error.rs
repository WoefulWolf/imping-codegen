//! Error and warning types.

use core::fmt;
use imping_ir::ValidationErrors;

/// Why [`Generator::generate`](crate::Generator::generate) failed.
///
/// Messages name the module, item or build involved.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The IR failed validation.
    InvalidIr(ValidationErrors),
    /// Two IR modules would get the same Rust module name.
    ModuleNameClash {
        /// The first IR module with that name.
        first: String,
        /// The second IR module with that name.
        second: String,
        /// The Rust module name both would get.
        rust_name: String,
    },
    /// [`Generator::module_name`](crate::Generator::module_name) or
    /// [`Generator::accept_build`](crate::Generator::accept_build) names a
    /// module that isn't in the IR.
    UnknownModule {
        /// The option that named it: `"module_name"` or `"accept_build"`.
        option: &'static str,
        /// The IR module name given.
        name: String,
    },
    /// [`Generator::module_name`](crate::Generator::module_name) was given a
    /// name that isn't a valid Rust module name.
    InvalidModuleName {
        /// The Rust module name given.
        name: String,
    },
    /// Two builds have the same label, or labels that make the same `Build`
    /// variant.
    DuplicateBuild {
        /// The second build's label.
        label: String,
    },
    /// A build's label isn't lowercase letters, digits and single
    /// underscores, starting with a letter.
    InvalidBuildLabel {
        /// The label given.
        label: String,
    },
    /// [`Generator::accept_build`](crate::Generator::accept_build) names a
    /// build label no IR has.
    UnknownBuild {
        /// The label given.
        label: String,
    },
    /// Two builds' IRs disagree on a type, a function's signature or a
    /// global's type, so one set of bindings can't fit both.
    BuildsDiffer(Box<BuildsDiffer>),
}

/// What two builds' IRs disagree on; see [`Error::BuildsDiffer`].
///
/// Its [`Display`](fmt::Display) is the error's message.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BuildsDiffer {
    /// The IR module the item is in.
    pub module: String,
    /// The item, such as ``type `cVec4` `` or ``function `Entity::Update` ``.
    pub item: String,
    /// The label of the build whose IR has it first.
    pub first: String,
    /// The label of the build that disagrees.
    pub second: String,
    /// How they differ, such as `size 0x10 vs 0x20`.
    pub difference: String,
}

impl fmt::Display for BuildsDiffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let BuildsDiffer {
            module,
            item,
            first,
            second,
            difference,
        } = self;
        write!(
            f,
            "`{module}`: {item} differs between builds `{first}` and `{second}`: {difference}"
        )
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidIr(errors) => write!(f, "the IR is invalid: {errors}"),
            Error::ModuleNameClash {
                first,
                second,
                rust_name,
            } => write!(
                f,
                "IR modules `{first}` and `{second}` both become Rust module `{rust_name}`; rename one"
            ),
            Error::UnknownModule { option, name } => {
                write!(f, "`{option}` names `{name}`, which is not in the IR")
            }
            Error::InvalidModuleName { name } => {
                write!(f, "`{name}` is not a valid Rust module name")
            }
            Error::DuplicateBuild { label } => {
                write!(
                    f,
                    "build `{label}` is given twice, or as another label's `Build` variant"
                )
            }
            Error::InvalidBuildLabel { label } => write!(
                f,
                "`{label}` is not a valid build label; use lowercase letters, digits and single \
                 underscores, starting with a letter, as in `steam_2021`"
            ),
            Error::UnknownBuild { label } => {
                write!(f, "`accept_build` names build `{label}`, which no IR has")
            }
            Error::BuildsDiffer(differ) => differ.fmt(f),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Error::InvalidIr(errors) => Some(errors),
            _ => None,
        }
    }
}

/// Something the generator skipped or simplified.
///
/// Warnings never stop generation. Their [`Display`](fmt::Display) names the
/// module and item.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Warning {
    /// A function uses custom argument storage, so only its RVA is generated.
    CustomStorage {
        /// The IR module the function is in.
        module: String,
        /// The function's name, including its namespace.
        function: String,
    },
    /// A struct or union's layout can't be represented in Rust, so it was
    /// emitted as opaque bytes of its size.
    TypeMadeOpaque {
        /// The IR module the type is in.
        module: String,
        /// The type's IR name.
        type_name: String,
        /// Why its layout can't be represented.
        reason: String,
    },
    /// A function was skipped.
    FunctionSkipped {
        /// The IR module the function is in.
        module: String,
        /// The function's name, including its namespace.
        function: String,
        /// Why it was skipped.
        reason: String,
    },
    /// A function can be called but not hooked, so it has no `hook`.
    ///
    /// Not raised for variadic functions, which never get a hook.
    HookUnavailable {
        /// The IR module the function is in.
        module: String,
        /// The function's name, including its namespace.
        function: String,
        /// Why it can't be hooked.
        reason: String,
    },
    /// A function is tagged `imping-safe`, but a parameter has no safe form,
    /// so its call wrapper stays `unsafe`.
    SafeCallUnavailable {
        /// The IR module the function is in.
        module: String,
        /// The function's name, including its namespace.
        function: String,
        /// Why the call can't be safe.
        reason: String,
    },
    /// An enum constant's value fits neither the signed nor the unsigned
    /// range of the enum's underlying type, so it was truncated to fit.
    ///
    /// A value that fits the other signedness of the same width (such as -1
    /// in a `u32` enum) wraps as in C, and is not reported.
    EnumValueTruncated {
        /// The IR module the enum is in.
        module: String,
        /// The enum's IR name.
        enum_name: String,
        /// The constant's IR name.
        variant: String,
        /// The constant's value in the IR.
        value: i64,
    },
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Warning::CustomStorage { module, function } => write!(
                f,
                "`{module}` function `{function}` uses custom storage; only its RVA is generated"
            ),
            Warning::TypeMadeOpaque {
                module,
                type_name,
                reason,
            } => write!(
                f,
                "`{module}` type `{type_name}` emitted as opaque bytes: {reason}"
            ),
            Warning::FunctionSkipped {
                module,
                function,
                reason,
            } => write!(f, "`{module}` function `{function}` skipped: {reason}"),
            Warning::HookUnavailable {
                module,
                function,
                reason,
            } => write!(
                f,
                "`{module}` function `{function}` can be called but not hooked: {reason}"
            ),
            Warning::SafeCallUnavailable {
                module,
                function,
                reason,
            } => write!(
                f,
                "`{module}` function `{function}` is tagged `imping-safe` but its call stays \
                 unsafe: {reason}"
            ),
            Warning::EnumValueTruncated {
                module,
                enum_name,
                variant,
                value,
            } => write!(
                f,
                "`{module}` enum `{enum_name}` constant `{variant}` = {value} does not fit \
                 the underlying type and was truncated"
            ),
        }
    }
}
