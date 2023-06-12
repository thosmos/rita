use std::{
    error::Error,
    fmt::{Display, Formatter, Result as FmtResult},
    time::SystemTimeError,
};

use althea_kernel_interface::KernelInterfaceError;
use awc::error::{JsonPayloadError, SendRequestError};
use babel_monitor::structs::BabelMonitorError;
use compressed_log::builder::LoggerError;
use log::SetLoggerError;
use settings::SettingsError;
use std::boxed::Box;

#[derive(Debug)]
pub enum RitaCommonError {
    ConversionError(String),
    LoggerError(LoggerError),
    SetLoggerError(SetLoggerError),
    UCIError(KernelInterfaceError),
    ToggleError(String),
    NicknameError(String),
    SettingsError(SettingsError),
    CapacityError(String),
    MiscStringError(String),
    KernelInterfaceError(KernelInterfaceError),
    StdError(std::io::Error),
    Lowest20Error(usize),
    BabelMonitorError(BabelMonitorError),
    SysTimeError(SystemTimeError),
    OldSendRequestError(String),
    BincodeError(Box<bincode::ErrorKind>),
    SendRequestError(SendRequestError),
    JsonPayloadError(JsonPayloadError),
    DuplicatePayment,
    PaymentFailed(String),
}

impl From<LoggerError> for RitaCommonError {
    fn from(error: LoggerError) -> Self {
        RitaCommonError::LoggerError(error)
    }
}
impl From<SetLoggerError> for RitaCommonError {
    fn from(error: SetLoggerError) -> Self {
        RitaCommonError::SetLoggerError(error)
    }
}
impl From<SettingsError> for RitaCommonError {
    fn from(error: SettingsError) -> Self {
        RitaCommonError::SettingsError(error)
    }
}
impl From<KernelInterfaceError> for RitaCommonError {
    fn from(error: KernelInterfaceError) -> Self {
        RitaCommonError::KernelInterfaceError(error)
    }
}
impl From<std::io::Error> for RitaCommonError {
    fn from(error: std::io::Error) -> Self {
        RitaCommonError::StdError(error)
    }
}

impl From<BabelMonitorError> for RitaCommonError {
    fn from(error: BabelMonitorError) -> Self {
        RitaCommonError::BabelMonitorError(error)
    }
}
impl From<SystemTimeError> for RitaCommonError {
    fn from(error: SystemTimeError) -> Self {
        RitaCommonError::SysTimeError(error)
    }
}
impl From<std::boxed::Box<bincode::ErrorKind>> for RitaCommonError {
    fn from(error: std::boxed::Box<bincode::ErrorKind>) -> Self {
        RitaCommonError::BincodeError(error)
    }
}
impl From<SendRequestError> for RitaCommonError {
    fn from(error: SendRequestError) -> Self {
        RitaCommonError::SendRequestError(error)
    }
}
impl From<JsonPayloadError> for RitaCommonError {
    fn from(error: JsonPayloadError) -> Self {
        RitaCommonError::JsonPayloadError(error)
    }
}

impl Display for RitaCommonError {
    fn fmt(&self, f: &mut Formatter) -> FmtResult {
        match self {
            RitaCommonError::ConversionError(a) => write!(
                f, "Conversion Error: {a}",
            ),
            RitaCommonError::LoggerError(e) => write!(f, "{e}"),
            RitaCommonError::SetLoggerError(e) => write!(f, "{e}"),
            RitaCommonError::UCIError(a) => write!(f, "{a}",),
            RitaCommonError::ToggleError(a) => write!(
                f, "Toggle Error: {a}",
            ),
            RitaCommonError::NicknameError(a) => write!(
                f, "Nickname Error: {a}",
            ),
            RitaCommonError::SettingsError(a) => write!(f, "{a}",),
            RitaCommonError::CapacityError(a) => write!(
                f, "Capacity Error: {a}",
            ),
            RitaCommonError::MiscStringError(a) => write!(f, "{a}",),
            RitaCommonError::PaymentFailed(a) => write!(f, "{a}",),
            RitaCommonError::DuplicatePayment => write!(f, "Duplicated payment!",),
            RitaCommonError::KernelInterfaceError(a) => write!(f, "{a}",),
            RitaCommonError::StdError(a) => write!(f, "{a}",),
            RitaCommonError::Lowest20Error(a) => write!(
                f, "There is no entry at index {a}, should not reach this condition, error with GAS_PRICES vecDeque logic",
            ),
            RitaCommonError::BabelMonitorError(a) => write!(f, "{a}",),
            RitaCommonError::SysTimeError(a) => write!(f, "{a}",),
            RitaCommonError::OldSendRequestError(e) => write!(f, "{e}"),
            RitaCommonError::BincodeError(e) => write!(f, "{e}"),
            RitaCommonError::SendRequestError(e) => write!(f, "{e}"),
            RitaCommonError::JsonPayloadError(e) => write!(f, "{e}"),
        }
    }
}

impl Error for RitaCommonError {}
