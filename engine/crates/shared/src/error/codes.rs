use serde::{Deserialize, Serialize};
use std::convert::TryFrom;

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ErrorCode {
    Ok = 0,

    // General (1..99)
    Internal = 1,
    InvalidArgument = 2,
    NotFound = 3,
    PermissionDenied = 4,
    Timeout = 5,
    Unsupported = 6,
    NotImplemented = 7,
    Cancelled = 8,

    /// Cross-thread / channel disconnected, sender dropped, etc.
    Disconnected = 9,
    InvalidOperation = 10,
    /// Bounded input transport refused an event. The host may retry later.
    InputSaturated = 11,

    // IO / FS (100..199)
    IoError = 100,
    BadFileDescriptor = 101,
    ExceedMaxConcurrentFdLimit = 102,

    // V8 / JS / Buffer (200..299)
    ArrayBufferDoesNotExist = 200,
    JsException = 201,
    ModuleLoadError = 202,
    /// V8 heap limit reached (near-heap-limit triggered).
    OutOfMemory = 203,
    /// JS execution exceeded the configured watchdog timeout.
    JsExecutionTimeout = 204,
    /// Host thread panicked (Rust panic).
    HostPanic = 205,
    /// ANR: Host thread stopped responding within the watchdog timeout.
    Anr = 206,
    /// Code manifest signature is invalid (Ed25519 verification failed).
    CodeSignatureInvalid = 207,
    /// Code integrity check failed (SHA256 hash mismatch).
    CodeIntegrityFailed = 208,

    // Image (300..399)
    ImageReadError = 300,
    InvalidImageBuffer = 301,

    // Render / GL (legacy/general) (400..402)
    RenderBackendError = 400,
    ShaderCompileError = 401,
    ProgramLinkError = 402,

    // Render backend / platform / context / surface (generic, backend-agnostic) (403..419)
    RenderLibraryLoadError = 403,
    RenderSymbolLoadError = 404,
    RenderBindApiError = 405,
    RenderGetDisplayError = 406,
    RenderInitializeError = 407,
    RenderChooseConfigError = 408,
    RenderCreateSurfaceError = 409,
    RenderCreateContextError = 410,
    RenderMakeCurrentError = 411,
    RenderSwapIntervalError = 412,
    /// Presenting a frame failed, and it was not a lost context. The session goes on: the
    /// render loop presents again next frame, and a Surface it can no longer present to is
    /// retired and reported as a lost Surface.
    RenderSwapBuffersError = 413,
    RenderInvalidStateError = 414,
    /// The framebuffer bound for the operation is not framebuffer complete.
    /// Distinct from [`Self::RenderInvalidStateError`] because WebGL has a
    /// dedicated error for it, and a readback that folded it into the generic
    /// invalid-operation code told content nothing about which of the two the
    /// driver actually refused.
    RenderFramebufferIncomplete = 415,

    // 2D / Canvas / Vector-graphics subsystem (generic) (420..429)
    Render2DInitError = 420,
    Render2DResourceError = 421,

    // Content's render commands (430..439)
    /// A render command the content issued -- a 2D draw, a GL call, a canvas change -- could not
    /// be executed. The render thread and the session carry on.
    ///
    /// Reported to a host as itself, with the command's own error in the detail. A host tells
    /// an error it can ride out from one that ended the game by its code, and the command's own
    /// code is one fatal reports carry too -- an invalid argument, a shader that did not
    /// compile, an internal error -- so a GL call the content got wrong reached Android hosts as
    /// non-recoverable.
    RenderCommandFailed = 430,
}

impl ErrorCode {
    #[inline]
    pub const fn default_message(self) -> &'static str {
        match self {
            ErrorCode::Ok => "ok",

            ErrorCode::Internal => "internal error",
            ErrorCode::InvalidArgument => "invalid argument",
            ErrorCode::NotFound => "not found",
            ErrorCode::PermissionDenied => "permission denied",
            ErrorCode::Timeout => "timeout",
            ErrorCode::Unsupported => "unsupported",
            ErrorCode::NotImplemented => "not implemented",
            ErrorCode::Cancelled => "cancelled",
            ErrorCode::Disconnected => "disconnected",
            ErrorCode::InvalidOperation => "invalid operation",
            ErrorCode::InputSaturated => "input transport saturated",

            ErrorCode::IoError => "io error",
            ErrorCode::BadFileDescriptor => "bad file descriptor",
            ErrorCode::ExceedMaxConcurrentFdLimit => "too many open files",

            ErrorCode::ArrayBufferDoesNotExist => "array buffer does not exist",
            ErrorCode::JsException => "js exception",
            ErrorCode::ModuleLoadError => "module load error",
            ErrorCode::OutOfMemory => "out of memory",
            ErrorCode::JsExecutionTimeout => "js execution timeout",
            ErrorCode::HostPanic => "host thread panic",
            ErrorCode::Anr => "application not responding",
            ErrorCode::CodeSignatureInvalid => "code signature invalid",
            ErrorCode::CodeIntegrityFailed => "code integrity check failed",

            ErrorCode::ImageReadError => "image read error",
            ErrorCode::InvalidImageBuffer => "invalid image buffer",

            ErrorCode::RenderBackendError => "render backend error",
            ErrorCode::ShaderCompileError => "shader compile error",
            ErrorCode::ProgramLinkError => "program link error",

            ErrorCode::RenderLibraryLoadError => "render library load error",
            ErrorCode::RenderSymbolLoadError => "render symbol load error",
            ErrorCode::RenderBindApiError => "render bind api error",
            ErrorCode::RenderGetDisplayError => "render get display error",
            ErrorCode::RenderInitializeError => "render initialize error",
            ErrorCode::RenderChooseConfigError => "render choose config error",
            ErrorCode::RenderCreateSurfaceError => "render create surface error",
            ErrorCode::RenderCreateContextError => "render create context error",
            ErrorCode::RenderMakeCurrentError => "render make current error",
            ErrorCode::RenderSwapIntervalError => "render swap interval error",
            ErrorCode::RenderSwapBuffersError => "render swap buffers error",
            ErrorCode::RenderInvalidStateError => "render invalid state error",
            ErrorCode::RenderFramebufferIncomplete => "framebuffer incomplete",

            ErrorCode::Render2DInitError => "render 2d init error",
            ErrorCode::Render2DResourceError => "render 2d resource error",

            ErrorCode::RenderCommandFailed => "render command failed",
        }
    }

    #[inline]
    pub const fn as_u16(self) -> u16 {
        self as u16
    }
}

impl From<ErrorCode> for u16 {
    #[inline]
    fn from(code: ErrorCode) -> Self {
        code.as_u16()
    }
}

impl TryFrom<u16> for ErrorCode {
    type Error = ();

    #[inline]
    fn try_from(v: u16) -> Result<Self, Self::Error> {
        let code = match v {
            0 => ErrorCode::Ok,

            1 => ErrorCode::Internal,
            2 => ErrorCode::InvalidArgument,
            3 => ErrorCode::NotFound,
            4 => ErrorCode::PermissionDenied,
            5 => ErrorCode::Timeout,
            6 => ErrorCode::Unsupported,
            7 => ErrorCode::NotImplemented,
            8 => ErrorCode::Cancelled,
            9 => ErrorCode::Disconnected,
            10 => ErrorCode::InvalidOperation,
            11 => ErrorCode::InputSaturated,

            100 => ErrorCode::IoError,
            101 => ErrorCode::BadFileDescriptor,
            102 => ErrorCode::ExceedMaxConcurrentFdLimit,

            200 => ErrorCode::ArrayBufferDoesNotExist,
            201 => ErrorCode::JsException,
            202 => ErrorCode::ModuleLoadError,
            203 => ErrorCode::OutOfMemory,
            204 => ErrorCode::JsExecutionTimeout,
            205 => ErrorCode::HostPanic,
            206 => ErrorCode::Anr,
            207 => ErrorCode::CodeSignatureInvalid,
            208 => ErrorCode::CodeIntegrityFailed,

            300 => ErrorCode::ImageReadError,
            301 => ErrorCode::InvalidImageBuffer,

            400 => ErrorCode::RenderBackendError,
            401 => ErrorCode::ShaderCompileError,
            402 => ErrorCode::ProgramLinkError,

            403 => ErrorCode::RenderLibraryLoadError,
            404 => ErrorCode::RenderSymbolLoadError,
            405 => ErrorCode::RenderBindApiError,
            406 => ErrorCode::RenderGetDisplayError,
            407 => ErrorCode::RenderInitializeError,
            408 => ErrorCode::RenderChooseConfigError,
            409 => ErrorCode::RenderCreateSurfaceError,
            410 => ErrorCode::RenderCreateContextError,
            411 => ErrorCode::RenderMakeCurrentError,
            412 => ErrorCode::RenderSwapIntervalError,
            413 => ErrorCode::RenderSwapBuffersError,
            414 => ErrorCode::RenderInvalidStateError,
            415 => ErrorCode::RenderFramebufferIncomplete,

            420 => ErrorCode::Render2DInitError,
            421 => ErrorCode::Render2DResourceError,

            430 => ErrorCode::RenderCommandFailed,

            _ => return Err(()),
        };
        Ok(code)
    }
}

/// std::io::ErrorKind is Copy, so `ErrorCode::from(e.kind())` is correct.
impl From<std::io::ErrorKind> for ErrorCode {
    #[inline]
    fn from(kind: std::io::ErrorKind) -> Self {
        use std::io::ErrorKind::*;
        match kind {
            NotFound => ErrorCode::NotFound,
            PermissionDenied => ErrorCode::PermissionDenied,
            TimedOut => ErrorCode::Timeout,
            Unsupported => ErrorCode::Unsupported,
            InvalidInput | InvalidData => ErrorCode::InvalidArgument,
            _ => ErrorCode::IoError,
        }
    }
}

#[inline]
pub fn io_error_to_error_code(e: &std::io::Error) -> ErrorCode {
    ErrorCode::from(e.kind())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_code_conversion_ok() {
        assert_eq!(ErrorCode::try_from(0u16), Ok(ErrorCode::Ok));
    }

    #[test]
    fn test_error_code_conversion_general() {
        assert_eq!(ErrorCode::try_from(1u16), Ok(ErrorCode::Internal));
        assert_eq!(ErrorCode::try_from(2u16), Ok(ErrorCode::InvalidArgument));
        assert_eq!(ErrorCode::try_from(3u16), Ok(ErrorCode::NotFound));
        assert_eq!(ErrorCode::try_from(4u16), Ok(ErrorCode::PermissionDenied));
        assert_eq!(ErrorCode::try_from(5u16), Ok(ErrorCode::Timeout));
        assert_eq!(ErrorCode::try_from(6u16), Ok(ErrorCode::Unsupported));
        assert_eq!(ErrorCode::try_from(7u16), Ok(ErrorCode::NotImplemented));
        assert_eq!(ErrorCode::try_from(8u16), Ok(ErrorCode::Cancelled));
        assert_eq!(ErrorCode::try_from(9u16), Ok(ErrorCode::Disconnected));
        assert_eq!(ErrorCode::try_from(10u16), Ok(ErrorCode::InvalidOperation));
        assert_eq!(ErrorCode::try_from(11u16), Ok(ErrorCode::InputSaturated));
    }

    #[test]
    fn test_error_code_conversion_io() {
        assert_eq!(ErrorCode::try_from(100u16), Ok(ErrorCode::IoError));
        assert_eq!(
            ErrorCode::try_from(101u16),
            Ok(ErrorCode::BadFileDescriptor)
        );
    }

    #[test]
    fn test_error_code_conversion_invalid() {
        assert_eq!(ErrorCode::try_from(9999u16), Err(()));
        assert_eq!(ErrorCode::try_from(50u16), Err(()));
        assert_eq!(ErrorCode::try_from(150u16), Err(()));
    }

    #[test]
    fn test_error_code_roundtrip() {
        let codes = [
            ErrorCode::Ok,
            ErrorCode::Internal,
            ErrorCode::NotFound,
            ErrorCode::IoError,
            ErrorCode::JsException,
            ErrorCode::RenderBackendError,
        ];

        for code in codes {
            let as_u16: u16 = code.into();
            let back = ErrorCode::try_from(as_u16).unwrap();
            assert_eq!(code, back);
        }
    }

    /// The engine's codes cross into Android as the SDK's `NATIVE_*` constants, which Java
    /// declares again by hand; its `isRecoverable` reads them. Each one is the Rust code of the
    /// same name and number, so neither side can renumber or rename one alone.
    #[test]
    fn every_native_code_the_android_sdk_declares_is_the_engines() {
        let java = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../platforms/android/library/src/main/java/com/migo/runtime/ErrorCode.java"
        ))
        .expect("the Android SDK's ErrorCode.java");
        let mut seen = 0;
        for line in java.lines() {
            let Some(rest) = line.trim().strip_prefix("public static final int NATIVE_") else {
                continue;
            };
            let (name, value) = rest.split_once('=').expect("NATIVE_X = N;");
            let value: u16 = value
                .trim()
                .trim_end_matches(';')
                .parse()
                .expect("a number");
            let code = ErrorCode::try_from(value)
                .unwrap_or_else(|()| panic!("NATIVE_{} = {value} is no engine code", name.trim()));
            let rust: String = format!("{code:?}")
                .chars()
                .enumerate()
                .flat_map(|(i, c)| {
                    let sep = (i > 0 && c.is_ascii_uppercase()).then_some('_');
                    sep.into_iter()
                        .chain(std::iter::once(c.to_ascii_uppercase()))
                })
                .collect();
            assert_eq!(
                name.trim(),
                rust,
                "NATIVE_{} = {value} is the engine's {code:?}",
                name.trim()
            );
            seen += 1;
        }
        assert!(
            seen >= 8,
            "read {seen} NATIVE_ constants; the declaration changed shape"
        );
    }

    #[test]
    fn test_error_code_default_message() {
        assert_eq!(ErrorCode::Ok.default_message(), "ok");
        assert_eq!(ErrorCode::NotFound.default_message(), "not found");
        assert_eq!(ErrorCode::Internal.default_message(), "internal error");
    }

    /// Pin every variant's `as u16` value so accidental
    /// reorders or renumbers surface as a failing test rather
    /// than a wire-format compatibility break.  The `ErrorCode`
    /// enum is Serialize+Deserialize and its numeric
    /// representation leaks into `RenderMetricsSnapshot`,
    /// Android JNI error callbacks, and persisted crash dumps —
    /// silent renumbers would misclassify historical incidents
    /// and break alerting rules keyed on specific codes.
    #[test]
    fn stable_error_code_numeric_values() {
        // Group assertions by class for readable diffs when a
        // new variant is added.
        assert_eq!(ErrorCode::Ok as u16, 0);

        // General 1..=11
        assert_eq!(ErrorCode::Internal as u16, 1);
        assert_eq!(ErrorCode::InvalidArgument as u16, 2);
        assert_eq!(ErrorCode::NotFound as u16, 3);
        assert_eq!(ErrorCode::PermissionDenied as u16, 4);
        assert_eq!(ErrorCode::Timeout as u16, 5);
        assert_eq!(ErrorCode::Unsupported as u16, 6);
        assert_eq!(ErrorCode::NotImplemented as u16, 7);
        assert_eq!(ErrorCode::Cancelled as u16, 8);
        assert_eq!(ErrorCode::Disconnected as u16, 9);
        assert_eq!(ErrorCode::InvalidOperation as u16, 10);
        assert_eq!(ErrorCode::InputSaturated as u16, 11);

        // IO / FS
        assert_eq!(ErrorCode::IoError as u16, 100);
        assert_eq!(ErrorCode::BadFileDescriptor as u16, 101);
        assert_eq!(ErrorCode::ExceedMaxConcurrentFdLimit as u16, 102);

        // V8 / JS / Buffer
        assert_eq!(ErrorCode::ArrayBufferDoesNotExist as u16, 200);
        assert_eq!(ErrorCode::JsException as u16, 201);
        assert_eq!(ErrorCode::ModuleLoadError as u16, 202);
        assert_eq!(ErrorCode::OutOfMemory as u16, 203);
        assert_eq!(ErrorCode::JsExecutionTimeout as u16, 204);
        assert_eq!(ErrorCode::HostPanic as u16, 205);
        assert_eq!(ErrorCode::Anr as u16, 206);
        assert_eq!(ErrorCode::CodeSignatureInvalid as u16, 207);
        assert_eq!(ErrorCode::CodeIntegrityFailed as u16, 208);

        // Image
        assert_eq!(ErrorCode::ImageReadError as u16, 300);
        assert_eq!(ErrorCode::InvalidImageBuffer as u16, 301);

        // Render / GL
        assert_eq!(ErrorCode::RenderBackendError as u16, 400);
        assert_eq!(ErrorCode::ShaderCompileError as u16, 401);
        assert_eq!(ErrorCode::ProgramLinkError as u16, 402);
        assert_eq!(ErrorCode::RenderLibraryLoadError as u16, 403);
        assert_eq!(ErrorCode::RenderSymbolLoadError as u16, 404);
        assert_eq!(ErrorCode::RenderBindApiError as u16, 405);
        assert_eq!(ErrorCode::RenderGetDisplayError as u16, 406);
        assert_eq!(ErrorCode::RenderInitializeError as u16, 407);
        assert_eq!(ErrorCode::RenderChooseConfigError as u16, 408);
        assert_eq!(ErrorCode::RenderCreateSurfaceError as u16, 409);
        assert_eq!(ErrorCode::RenderCreateContextError as u16, 410);
        assert_eq!(ErrorCode::RenderMakeCurrentError as u16, 411);
        assert_eq!(ErrorCode::RenderSwapIntervalError as u16, 412);
        assert_eq!(ErrorCode::RenderSwapBuffersError as u16, 413);
        assert_eq!(ErrorCode::RenderInvalidStateError as u16, 414);

        // 2D / Canvas
        assert_eq!(ErrorCode::Render2DInitError as u16, 420);
        assert_eq!(ErrorCode::Render2DResourceError as u16, 421);

        // Content's render commands
        assert_eq!(ErrorCode::RenderCommandFailed as u16, 430);
    }

    #[test]
    fn test_io_error_kind_conversion() {
        use std::io::ErrorKind;

        assert_eq!(ErrorCode::from(ErrorKind::NotFound), ErrorCode::NotFound);
        assert_eq!(
            ErrorCode::from(ErrorKind::PermissionDenied),
            ErrorCode::PermissionDenied
        );
        assert_eq!(ErrorCode::from(ErrorKind::TimedOut), ErrorCode::Timeout);
        assert_eq!(
            ErrorCode::from(ErrorKind::InvalidInput),
            ErrorCode::InvalidArgument
        );
        assert_eq!(ErrorCode::from(ErrorKind::Other), ErrorCode::IoError);
    }
}
