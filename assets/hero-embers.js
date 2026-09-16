(() => {
    window.__thermiteEmbersCleanup?.();
    const canvas = document.getElementById('hero-embers');
    const mark = document.querySelector('#hero-brand svg');
    const ctx = canvas?.getContext('2d');
    if (!ctx || !mark) return;

    const reduced = matchMedia('(prefers-reduced-motion: reduce)');
    const listeners = new AbortController();
    const dark = () => document.documentElement.getAttribute('data-theme') !== 'light';
    let width = 0, height = 0, frame = 0, last = 0, time = 0, pending = 0;
    let visible = true, disposed = false;
    const embers = [];
    // Dark theme only for now: the embers are drawn additively, which washes out to
    // white on a near-white base. A light palette is a second set of stops, not a rewrite.
    const moving = () => !disposed && !reduced.matches && visible && !document.hidden && dark();

    // Sparks off a thermite reaction, thrown from the mark's white-hot core: most drift
    // up as embers, a few shoot out fast and fall. Heat is age — white → gold → ember → out.
    const RATE = 42, MAX = 160;
    const HEAT = ['#fffdf7', '#ffe08a', '#ffc233', '#ff8a1f', '#e2401f', '#7a2412'];

    function emit(left, top, s) {
        const spark = Math.random() < 0.2;
        const angle = -Math.PI / 2 + (Math.random() - 0.5) * (spark ? 1.6 : 0.5);
        const speed = spark ? 120 + Math.random() * 160 : 30 + Math.random() * 50;
        embers.push({
            // The core triangle sits at (12, 15) in the mark's 24-unit viewBox.
            x: left + 12 * s + (Math.random() - 0.5) * 5 * s,
            y: top + 15 * s + (Math.random() - 0.5) * 3 * s,
            vx: Math.cos(angle) * speed,
            vy: Math.sin(angle) * speed,
            spark,
            age: 0,
            life: spark ? 0.5 + Math.random() * 0.5 : 1.4 + Math.random() * 1.4,
            size: spark ? 0.8 + Math.random() * 0.8 : 1.1 + Math.random() * 1.3,
            phase: Math.random() * Math.PI * 2,
            sway: 2 + Math.random() * 3,
        });
    }

    function step(dt) {
        for (let i = embers.length - 1; i >= 0; i--) {
            const p = embers[i];
            p.age += dt / p.life;
            if (p.age >= 1) {
                embers[i] = embers[embers.length - 1];
                embers.pop();
                continue;
            }
            if (p.spark) {
                p.vy += 260 * dt;
                p.vx *= 1 - 1.4 * dt;
            } else {
                p.vy -= 12 * dt;
                p.vx += Math.sin(time * p.sway + p.phase) * 40 * dt;
                p.vx *= 1 - 1.5 * dt;
            }
            p.x += p.vx * dt;
            p.y += p.vy * dt;
        }
    }

    function draw() {
        ctx.clearRect(0, 0, width, height);
        ctx.globalCompositeOperation = 'lighter';
        for (const p of embers) {
            const fade = p.age < 0.1 ? p.age / 0.1 : 1 - (p.age - 0.1) / 0.9;
            const alpha = fade * (0.75 + 0.25 * Math.sin(time * 18 + p.phase));
            const r = p.size * (1 - 0.5 * p.age);
            ctx.fillStyle = HEAT[Math.min(HEAT.length - 1, p.age * HEAT.length | 0)];
            // A soft halo under each ember is what makes it glow instead of sitting on the page.
            ctx.globalAlpha = alpha * 0.14;
            ctx.beginPath();
            ctx.arc(p.x, p.y, r * 3.2, 0, Math.PI * 2);
            ctx.fill();
            ctx.globalAlpha = alpha;
            ctx.beginPath();
            ctx.arc(p.x, p.y, r, 0, Math.PI * 2);
            ctx.fill();
        }
        ctx.globalAlpha = 1;
        ctx.globalCompositeOperation = 'source-over';
    }

    function tick(now) {
        frame = 0;
        if (!moving()) return;
        const dt = last ? Math.min((now - last) / 1000, 0.033) : 0;
        last = now;
        time += dt;
        // Read the mark every frame: it is still rising through its entrance animation.
        const m = mark.getBoundingClientRect();
        const c = canvas.getBoundingClientRect();
        // The reaction warms up with the mark's entrance; sparks over an invisible mark read as a bug.
        pending = Math.min(pending + dt * RATE * Math.min(1, time / 1.5), 4);
        for (; pending >= 1 && embers.length < MAX; pending--) emit(m.left - c.left, m.top - c.top, m.width / 24);
        step(dt);
        draw();
        frame = requestAnimationFrame(tick);
    }

    function resize() {
        width = canvas.clientWidth;
        height = canvas.clientHeight;
        const dpr = Math.min(devicePixelRatio || 1, 2);
        canvas.width = Math.round(width * dpr);
        canvas.height = Math.round(height * dpr);
        ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
        draw();
    }

    function sync() {
        cancelAnimationFrame(frame);
        frame = 0;
        last = 0;
        if (moving()) {
            frame = requestAnimationFrame(tick);
        } else if (reduced.matches || !dark()) {
            embers.length = 0;
            ctx.clearRect(0, 0, width, height);
        }
    }

    document.addEventListener('visibilitychange', sync, { signal: listeners.signal });
    reduced.addEventListener('change', sync, { signal: listeners.signal });
    const sizeObserver = new ResizeObserver(resize);
    sizeObserver.observe(canvas);
    const intersection = new IntersectionObserver(([entry]) => {
        visible = entry.isIntersecting;
        sync();
    });
    intersection.observe(canvas);
    const themeObserver = new MutationObserver(sync);
    themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });

    window.__thermiteEmbersCleanup = () => {
        disposed = true;
        cancelAnimationFrame(frame);
        listeners.abort();
        sizeObserver.disconnect();
        intersection.disconnect();
        themeObserver.disconnect();
        delete window.__thermiteEmbersCleanup;
    };
    resize();
    sync();
})();
