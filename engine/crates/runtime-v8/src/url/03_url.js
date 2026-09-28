// Parse a URL string into its components
function parseURL(url) {
    // protocol
    const protoEnd = url.indexOf('://');
    if (protoEnd === -1) {
        return { protocol: '', hostname: '', port: '', pathname: '/', search: '', hash: '', host: '', origin: '' };
    }
    const protocol = url.slice(0, protoEnd + 1); // e.g. "https:"

    let rest = url.slice(protoEnd + 3); // after "://"

    // hash
    let hash = '';
    const hashIdx = rest.indexOf('#');
    if (hashIdx !== -1) {
        hash = rest.slice(hashIdx);
        rest = rest.slice(0, hashIdx);
    }

    // search
    let search = '';
    const searchIdx = rest.indexOf('?');
    if (searchIdx !== -1) {
        search = rest.slice(searchIdx);
        rest = rest.slice(0, searchIdx);
    }

    // pathname
    let pathname = '/';
    const pathIdx = rest.indexOf('/');
    if (pathIdx !== -1) {
        pathname = rest.slice(pathIdx);
        rest = rest.slice(0, pathIdx);
    }

    // host (hostname:port)
    let hostname = rest;
    let port = '';
    const colonIdx = rest.lastIndexOf(':');
    if (colonIdx !== -1) {
        const possiblePort = rest.slice(colonIdx + 1);
        if (/^\d+$/.test(possiblePort)) {
            hostname = rest.slice(0, colonIdx);
            port = possiblePort;
        }
    }

    const host = port ? `${hostname}:${port}` : hostname;
    const origin = `${protocol}//${host}`;

    return { protocol, hostname, port, pathname, search, hash, host, origin };
}

class URL {
    _url = 'minigame://mini-game-host';
    _parts = null;

    constructor(url, base) {
        if (base !== undefined) {
            // Simple base URL resolution
            const baseStr = typeof base === 'string' ? base : base.toString();
            if (url.startsWith('//')) {
                const baseParts = parseURL(baseStr);
                url = baseParts.protocol + url;
            } else if (url.startsWith('/')) {
                const baseParts = parseURL(baseStr);
                url = baseParts.origin + url;
            } else if (!/^[a-zA-Z][a-zA-Z0-9+\-.]*:\/\//.test(url)) {
                const baseParts = parseURL(baseStr);
                const basePath = baseParts.pathname.slice(0, baseParts.pathname.lastIndexOf('/') + 1);
                url = baseParts.origin + basePath + url;
            }
        }
        this._url = url;
        this._parts = parseURL(url);
    }

    _reparse() {
        this._parts = parseURL(this._url);
    }

    get href() {
        return this._url;
    }

    set href(value) {
        this._url = value;
        this._reparse();
    }

    get hash() {
        return this._parts.hash;
    }

    get origin() {
        return this._parts.origin;
    }

    get protocol() {
        return this._parts.protocol;
    }

    get host() {
        return this._parts.host;
    }

    get hostname() {
        return this._parts.hostname;
    }

    get port() {
        return this._parts.port;
    }

    get pathname() {
        return this._parts.pathname;
    }

    get search() {
        return this._parts.search;
    }

    get searchParams() {
        // Minimal URLSearchParams-like object
        const params = new Map();
        if (this._parts.search) {
            const query = this._parts.search.slice(1); // remove '?'
            for (const pair of query.split('&')) {
                const eqIdx = pair.indexOf('=');
                if (eqIdx !== -1) {
                    params.set(decodeURIComponent(pair.slice(0, eqIdx)), decodeURIComponent(pair.slice(eqIdx + 1)));
                } else {
                    params.set(decodeURIComponent(pair), '');
                }
            }
        }
        return {
            get(key) { return params.get(key) ?? null; },
            has(key) { return params.has(key); },
            entries() { return params.entries(); },
            toString() {
                const parts = [];
                for (const [k, v] of params) {
                    parts.push(`${encodeURIComponent(k)}=${encodeURIComponent(v)}`);
                }
                return parts.join('&');
            }
        };
    }

    toString() {
        return this._url;
    }

    toJSON() {
        return this._url;
    }

    // Blob URL creation and revocation are not supported; no Blob type is present.
    static createObjectURL(blob) {
        throw new Error('createObjectURL is not supported in this environment');
    }

    static revokeObjectURL(url) {
        throw new Error('revokeObjectURL is not supported in this environment');
    }
}

export { URL }