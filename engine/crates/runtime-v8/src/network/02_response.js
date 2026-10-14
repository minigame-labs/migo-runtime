function nullBodyStatus(status) {
    return status === 101 || status === 204 || status === 205 || status === 304;
}

// `res.exception`: how many times the request was retried underneath, and why each attempt failed. Content reads
// `retryCount` and `reasons` off the object a callback receives, so they are its own properties, not only toJSON's.
class Exception {
    constructor(errno, errMsg, retryCount = 0) {
        this._retryCount = retryCount;
        this._reasons = [{ errMsg, errno }];
    }

    // A request that went through untroubled: the reference answers success with an exception too, an empty one.
    static none() {
        const exception = new Exception(0, "", 0);
        exception._reasons = [];
        return exception;
    }

    get retryCount() {
        return this._retryCount;
    }

    get reasons() {
        return this._reasons;
    }

    appendReason(errno, errMsg) {
        this._reasons.push({ errno, errMsg });
    }

    toJSON() {
        return {
            retryCount: this._retryCount,
            reasons: this._reasons
        };
    }
}

// -- ResponseBase --

class ResponseBase {
    constructor(useHttpDNS, errMsg, exception) {
        this._useHttpDNS = useHttpDNS;
        this._errMsg = errMsg;
        this._exception = exception;
        this._profile = undefined;
    }

    // What the request measured on its way (`enableProfile`, on by default): set once the body has been read.
    set profile(value) {
        this._profile = value;
    }

    get profile() {
        return this._profile;
    }

    get useHttpDNS() {
        return this._useHttpDNS;
    }

    get exception() {
        return this._exception;
    }

    // Callbacks receive this instance directly (not toJSON output), so
    // the common mini-game platform's standard `res.errMsg` must be a live getter, not just a
    // toJSON field. Same reason header/errno getters exist on the
    // subclasses below.
    get errMsg() {
        return this._errMsg;
    }

    toJSON() {
        const json = {
            useHttpDNS: this._useHttpDNS,
            errMsg: this._errMsg,
            exception: this._exception?.toJSON()
        };
        if (this._profile !== undefined) json.profile = this._profile;
        return json;
    }
}

// -- request --

const requestOkMsg = "request:ok";
const requestFailMsg = "request:fail";

class Response extends ResponseBase {
    constructor(header, useHttpDNS = false) {
        super(useHttpDNS, requestOkMsg, Exception.none());
        this._header = header;
        this._data = null;
    }

    set data(value) {
        this._data = value;
    }

    get data() {
        return this._data;
    }

    get statusCode() {
        return this._header?.statusCode;
    }

    get header() {
        return this._header?.headers;
    }

    get cookies() {
        return this._header?.cookies;
    }

    toJSON() {
        return {
            ...super.toJSON(),
            ...this._header?.toJSON(),
            data: this._data,
        };
    }
}

class ErrorResponse extends ResponseBase {
    constructor(errno, exception) {
        super(false, requestFailMsg, exception);
        this._errno = errno;
    }

    get errno() {
        return this._errno;
    }

    toJSON() {
        return {
            ...super.toJSON(),
            errno: this._errno,
        };
    }
}

function abortedNetworkError(retryCount = 0) {
    return new ErrorResponse(-1, new Exception(-1, "Request Aborted", retryCount));
}

function timedOutNetworkError(retryCount = 0) {
    return new ErrorResponse(408, new Exception(408, "Request Timed Out", retryCount));
}

// -- downloadFile --

const downloadOkMsg = "downloadFile:ok";
const downloadFailMsg = "downloadFile:fail";

class DownloadResponse extends ResponseBase {
    constructor(tempFilePath, filePath, statusCode) {
        super(false, downloadOkMsg, null);
        this._tempFilePath = tempFilePath;
        this._filePath = filePath;
        this._statusCode = statusCode;
    }

    get tempFilePath() {
        return this._tempFilePath;
    }

    get filePath() {
        return this._filePath;
    }

    get statusCode() {
        return this._statusCode;
    }

    toJSON() {
        return {
            ...super.toJSON(),
            tempFilePath: this._tempFilePath,
            filePath: this._filePath,
            statusCode: this._statusCode,
        };
    }
}

class DownloadErrorResponse extends ResponseBase {
    constructor(errno, exception) {
        super(false, downloadFailMsg, exception);
        this._errno = errno;
    }

    get errno() {
        return this._errno;
    }

    toJSON() {
        return {
            ...super.toJSON(),
            errno: this._errno,
        };
    }
}

// -- uploadFile --

const uploadOkMsg = "uploadFile:ok";
const uploadFailMsg = "uploadFile:fail";

class UploadResponse extends ResponseBase {
    constructor(data, statusCode) {
        super(false, uploadOkMsg, null);
        this._data = data;
        this._statusCode = statusCode;
    }

    get data() {
        return this._data;
    }

    get statusCode() {
        return this._statusCode;
    }

    toJSON() {
        return {
            ...super.toJSON(),
            data: this._data,
            statusCode: this._statusCode,
        };
    }
}

class UploadErrorResponse extends ResponseBase {
    constructor(errno, exception) {
        super(false, uploadFailMsg, exception);
        this._errno = errno;
    }

    get errno() {
        return this._errno;
    }

    toJSON() {
        return {
            ...super.toJSON(),
            errno: this._errno,
        };
    }
}

export {
    Response, ResponseBase, ErrorResponse,
    DownloadResponse, DownloadErrorResponse,
    UploadResponse, UploadErrorResponse,
    abortedNetworkError, timedOutNetworkError,
    nullBodyStatus, Exception,
};
