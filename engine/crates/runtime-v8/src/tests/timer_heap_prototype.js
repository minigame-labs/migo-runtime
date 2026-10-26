// Test-only alternative backend for a same-policy comparison. It is never an
// extension in the shipped runtime. One core timer wakes an indexed JS heap.
export function installHeapPrototype(core, now) {
    const native = {
        create: core.createTimer,
        cancel: core.cancelTimer,
        ref: core.refTimer,
        unref: core.unrefTimer,
    };
    const heap = [];
    let nextSequence = 0;
    let refedCount = 0;
    let wake = null;
    let wakeDeadline = -1;
    let dispatching = false;

    function before(a, b) {
        return a.deadline < b.deadline ||
            (a.deadline === b.deadline && a.sequence < b.sequence);
    }

    function swap(a, b) {
        const entry = heap[a];
        heap[a] = heap[b];
        heap[b] = entry;
        heap[a].index = a;
        heap[b].index = b;
    }

    function up(index) {
        while (index > 0) {
            const parent = (index - 1) >>> 1;
            if (!before(heap[index], heap[parent])) break;
            swap(index, parent);
            index = parent;
        }
        return index;
    }

    function down(index) {
        while (index * 2 + 1 < heap.length) {
            let child = index * 2 + 1;
            if (child + 1 < heap.length && before(heap[child + 1], heap[child])) child++;
            if (!before(heap[child], heap[index])) break;
            swap(index, child);
            index = child;
        }
    }

    function insert(entry) {
        entry.index = heap.length;
        heap.push(entry);
        up(entry.index);
    }

    function remove(entry) {
        const index = entry.index;
        if (index < 0) return;
        const last = heap.pop();
        entry.index = -1;
        if (index < heap.length) {
            heap[index] = last;
            last.index = index;
            down(up(index));
        }
    }

    function arm() {
        if (dispatching) return;
        const head = heap[0];
        if (head === undefined) {
            if (wake !== null) native.cancel(wake);
            wake = null;
            wakeDeadline = -1;
            return;
        }
        if (wake === null || wakeDeadline !== head.deadline) {
            if (wake !== null) native.cancel(wake);
            wakeDeadline = head.deadline;
            wake = native.create(dispatch, Math.max(0, Math.ceil((head.deadline - now()) / 1000)),
                undefined, false, refedCount > 0, false, true);
        }
        if (refedCount > 0) native.ref(wake);
        else native.unref(wake);
    }

    function dispatch() {
        wake = null;
        wakeDeadline = -1;
        dispatching = true;
        const lastSequence = nextSequence;
        const ready = [];
        // Capture the current ready batch. Timers created by callbacks wait
        // for another core task opportunity, even at zero delay.
        while (heap[0] !== undefined && heap[0].deadline <= now() &&
            heap[0].sequence <= lastSequence) {
            const entry = heap[0];
            remove(entry);
            ready.push(entry);
        }
        let cursor = 0;
        try {
            for (; cursor < ready.length; cursor++) {
                const entry = ready[cursor];
                if (entry.destroyed) continue;
                if (!entry.repeat) {
                    entry.destroyed = true;
                    if (entry.refed) refedCount--;
                }
                try {
                    entry.callback(...(entry.args ?? []));
                } finally {
                    if (entry.repeat && !entry.destroyed) {
                        entry.deadline = now() + entry.period;
                        entry.sequence = ++nextSequence;
                        insert(entry);
                    }
                }
            }
        } finally {
            // A throwing callback cannot strand the rest of its ready batch.
            for (let index = cursor + 1; index < ready.length; index++) {
                if (!ready[index].destroyed) insert(ready[index]);
            }
            dispatching = false;
            arm();
        }
    }

    core.createTimer = (callback, delay, args, repeat = false, refed = true,
        _system = false, allowZero = false) => {
        const period = Math.max(repeat || !allowZero ? 1 : 0, Math.trunc(delay)) * 1000;
        const entry = {
            callback, args, repeat, refed, period, destroyed: false,
            deadline: now() + period, sequence: ++nextSequence, index: -1,
        };
        if (refed) refedCount++;
        insert(entry);
        arm();
        return entry;
    };
    core.cancelTimer = (entry) => {
        if (entry.destroyed) return;
        entry.destroyed = true;
        if (entry.refed) refedCount--;
        remove(entry);
        arm();
    };
    core.refTimer = (entry) => {
        if (entry.destroyed || entry.refed) return;
        entry.refed = true;
        refedCount++;
        arm();
    };
    core.unrefTimer = (entry) => {
        if (entry.destroyed || !entry.refed) return;
        entry.refed = false;
        refedCount--;
        arm();
    };
}
