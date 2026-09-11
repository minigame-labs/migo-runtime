(() => {
    function check(condition, message) {
        if (!condition) throw new Error(message);
    }
    const ctx = new WebGL2RenderingContext({ _rid: 23, width: 2, height: 2 });
    function expectBytes(bytes, start, count) {
        for (let i = 0; i < bytes.length; ++i) {
            const expected = i >= start && i < start + count ? i - start + 1 : 165;
            check(bytes[i] === expected, 'unexpected byte ' + i + ': ' + bytes[i]);
        }
    }
    // Real view brands determine element size. Packed pixels do not change it.
    const cases = [
        [Uint8Array, ctx.RGBA, ctx.UNSIGNED_BYTE, 4],
        [Uint8ClampedArray, ctx.RGBA, ctx.UNSIGNED_BYTE, 4],
        [Uint16Array, ctx.RGB, ctx.UNSIGNED_SHORT_5_6_5, 2],
        [Uint32Array, 0x8D99 /* RGBA_INTEGER */, ctx.UNSIGNED_INT, 16],
        [Int32Array, 0x8D99 /* RGBA_INTEGER */, ctx.INT, 16],
        [Float32Array, ctx.RGBA, ctx.FLOAT, 16],
    ];
    for (const [Type, format, type, pixelBytes] of cases) {
        for (const Store of [ArrayBuffer, SharedArrayBuffer]) {
            const all = new Uint8Array(new Store(96)).fill(165);
            const view = new Type(all.buffer, 8, 64 / Type.BYTES_PER_ELEMENT);
            const offsetBytes = 3 * Type.BYTES_PER_ELEMENT;
            ctx.readPixels(0, 0, 1, 1, format, type, view, 3);
            check(ctx.getError() === ctx.NO_ERROR, 'valid offset rejected');
            expectBytes(all, 8 + offsetBytes, pixelBytes);
        }
    }
    // Normal numeric conversions, including 64-bit wrapping. An omitted
    // argument and explicit undefined both select offset zero.
    for (const [offset, expected] of [
        [undefined, 0], [null, 0], [NaN, 0], [Infinity, 0], [-Infinity, 0],
        [-0, 0], [-0.75, 0], [2.9, 2], ['3', 3], [true, 1],
        [2 ** 64, 0], [-(2 ** 64), 0],
    ]) {
        const bytes = new Uint8Array(16).fill(165);
        ctx.readPixels(0, 0, 1, 1, ctx.RGBA, ctx.UNSIGNED_BYTE, bytes, offset);
        check(ctx.getError() === ctx.NO_ERROR, 'converted offset rejected');
        expectBytes(bytes, expected, 4);
    }
    const omitted = new Uint8Array(8).fill(165);
    ctx.readPixels(0, 0, 1, 1, ctx.RGBA, ctx.UNSIGNED_BYTE, omitted);
    expectBytes(omitted, 0, 4);

    // Offset conversion may run user code. It must finish once, before
    // inspecting the view length, and thrown conversions must not dispatch.
    let coercions = 0;
    const coerced = new Uint8Array(8).fill(165);
    ctx.readPixels(0, 0, 1, 1, ctx.RGBA, ctx.UNSIGNED_BYTE, coerced,
        { valueOf() { ++coercions; return 2; } });
    check(coercions === 1, 'offset converted more than once');
    expectBytes(coerced, 2, 4);
    for (const offset of [1n, Symbol(), { valueOf() { throw new TypeError('test'); } }]) {
        let threw = false;
        try { ctx.readPixels(0, 0, 1, 1, ctx.RGBA, ctx.UNSIGNED_BYTE, coerced, offset); }
        catch (error) { threw = error instanceof TypeError; }
        check(threw, 'invalid numeric conversion did not throw');
        expectBytes(coerced, 2, 4);
    }
    const detached = new Uint8Array(8).fill(165);
    ctx.readPixels(0, 0, 1, 1, ctx.RGBA, ctx.UNSIGNED_BYTE, detached,
        { valueOf() { detached.buffer.transfer(); return 0; } });
    check(ctx.getError() === ctx.INVALID_OPERATION, 'detached view reached renderer');

    // Large offsets must never wrap at 32 bits or saturate to offset zero.
    for (const offset of [5, 8, 9, -1, -1.9, 2 ** 32, 2 ** 32 + 1, 2 ** 53,
        2 ** 64 - 2048, -(2 ** 64) + 2048]) {
        const bytes = new Uint8Array(8).fill(165);
        ctx.readPixels(0, 0, 1, 1, ctx.RGBA, ctx.UNSIGNED_BYTE, bytes, offset);
        check(ctx.getError() === ctx.INVALID_OPERATION, 'out-of-range offset accepted: ' + offset);
        expectBytes(bytes, 0, 0);
    }
    const empty = new Uint8Array(8).fill(165);
    ctx.readPixels(0, 0, 0, 1, ctx.RGBA, ctx.UNSIGNED_BYTE, empty, 8);
    check(ctx.getError() === ctx.NO_ERROR, 'empty read at end rejected');
    ctx.readPixels(0, 0, 0, 1, ctx.RGBA, ctx.UNSIGNED_BYTE, empty, 9);
    check(ctx.getError() === ctx.INVALID_OPERATION, 'empty read beyond end accepted');
    expectBytes(empty, 0, 0);

    // WebGL1 ignores the extra argument, including any side effects it could run.
    const ctx1 = new WebGLRenderingContext({ _rid: 24, width: 2, height: 2 }, {});
    const legacy = new Uint8Array(8).fill(165);
    ctx1.readPixels(0, 0, 1, 1, ctx1.RGBA, ctx1.UNSIGNED_BYTE, legacy,
        { valueOf() { throw new Error('WebGL1 converted extra argument'); } });
    expectBytes(legacy, 0, 4);
})();
