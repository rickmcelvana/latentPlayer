//! Writing WGSL out of a preset's shader tree.
//!
//! The half of decision 54 that needs types. [`parse`](super::parse) records
//! what was written; this decides what it *means*, because HLSL will silently
//! reshape a value in three places WGSL will not accept one:
//!
//! ```text
//! float  d   = texsize.zw;        // float2 -> float, dropping .y
//! float2 v   = 0.008;             // scalar -> both lanes
//! ret       -= (lum(ret)<x)*ret;  // bool   -> float, then float3
//! ```
//!
//! So every expression is emitted with its type, and every assignment,
//! argument and operand is fitted to the type its context wants using
//! [`ty::Fit`](super::ty::Fit). There is no point at which a string is
//! substituted for another string.
//!
//! # Four deliberate divergences from what MilkDrop compiles
//!
//! **`tex2D` becomes `textureSampleLevel`, never `textureSample`.** WGSL
//! forbids implicit-derivative sampling under non-uniform control flow, and a
//! quarter of the corpus branches -- a `tex2D` inside an `if` is entirely
//! ordinary in these files and would fail to compile otherwise. The feedback
//! and blur targets have one mip level, so an explicit LOD of zero reads what
//! `tex2D` read there anyway.
//!
//! **Every name a preset writes is prefixed `p_`.** A preset may declare a
//! function called `lum` or a variable called `loop`, and both would collide
//! -- one with the prelude, one with WGSL itself. Prefixing everything also
//! preserves HLSL's own behaviour that the environment's *macros* win over a
//! preset's function of the same name, since the macro is what the unprefixed
//! name still refers to.
//!
//! **Every loop carries a guard counter.** A preset is a stranger's program
//! and a runaway loop in a fragment shader is not a slow frame, it is a hung
//! GPU. Decision 51 bounds the equation interpreter for the same reason; this
//! is the same bound one layer out, and it costs an integer compare.
//!
//! **A texture this build does not load resolves to a placeholder** rather
//! than refusing the shader. Decision 56 keeps texture packs out of scope, and
//! 19.6% of the corpus asks for one; drawing approximately and *naming what is
//! missing* (which `parse::Unsupported` already does) beats drawing nothing.

use std::collections::{BTreeSet, HashMap};

use super::ast::*;
use super::env::{self, Pass};
use super::parse::HlslError;
use super::ty::{self, Base, Shape, Ty};

/// MilkDrop's shader environment as WGSL: the uniform block, the samplers and
/// textures, and the helper functions `include.fx` defines as macros.
pub const PRELUDE: &str = include_str!("prelude.wgsl");

/// The most iterations any one loop in a translated shader may run.
///
/// Not a performance limit -- it is what stands between a stranger's preset
/// and a hung display. Generous enough that no plausible preset reaches it:
/// the corpus's heaviest loop runs a few dozen times.
pub const MAX_LOOP: u32 = 4096;

/// A translated shader.
#[derive(Clone, Debug)]
pub struct Emitted {
    /// A complete WGSL module: the prelude, then the preset's own code.
    pub source: String,
    /// Textures the preset asked for by name that this build does not load,
    /// in the order first seen. Each one drew from the placeholder.
    pub wants_textures: Vec<String>,
}

/// Translate one pass of a preset's shader into WGSL.
pub fn emit(shader: &Shader, pass: Pass) -> Result<Emitted, HlslError> {
    let Some(body) = &shader.body else {
        return Err(HlslError {
            line: 1,
            construct: "no-shader-body",
            detail: "the shader has no `shader_body { ... }` block".into(),
        });
    };

    let mut e = Emitter {
        pass,
        scopes: Vec::new(),
        globals: HashMap::new(),
        functions: HashMap::new(),
        out: String::new(),
        depth: 1,
        loops: 0,
        temps: 0,
        mirrors: mirrored_registers(shader, pass),
        wants: BTreeSet::new(),
        wants_order: Vec::new(),
    };

    // Globals first: a function may use one, and so may the body.
    let mut init = String::new();
    for var in &shader.globals {
        e.global(var, &mut init)?;
    }
    // Then every function's signature, so calls can be typed before the
    // definitions are emitted -- a preset may call a helper declared later.
    for f in &shader.functions {
        e.functions.insert(
            f.name.clone(),
            (f.ret, f.params.iter().map(|p| p.ty).collect()),
        );
    }
    let mut functions = String::new();
    for f in &shader.functions {
        functions.push_str(&e.function(f)?);
    }

    let mut main = String::new();
    e.scopes.push(HashMap::new());
    e.declare("ret", Ty::FLOAT3);
    e.depth = 1;
    let statements = e.block_body(body)?;
    e.scopes.pop();

    // The parameters are copied into `var`s rather than used directly, for
    // two reasons that only a corpus reveals. HLSL's parameters are mutable
    // copies and WGSL's are immutable -- and presets assign to `uv` constantly
    // (`uv = 0.05 + 0.95*uv;` is how a preset insets the picture). Every one
    // of those would fail to compile against a parameter.
    main.push_str(
        "fn milk_shader(uv_in: vec4<f32>, rad_ang_in: vec2<f32>, diffuse_in: vec4<f32>) -> vec4<f32> {\n",
    );
    main.push_str("    var in_uv = uv_in;\n");
    main.push_str("    var in_rad_ang = rad_ang_in;\n");
    main.push_str("    var in_diffuse = diffuse_in;\n");
    let needs_init = !init.is_empty() || !e.mirrors.is_empty();
    if needs_init {
        main.push_str("    milk_init_globals();\n");
    }
    // MilkDrop's own first line: `float3 ret = 0;`. A body that never assigns
    // it draws black rather than the previous frame -- reaching that is what
    // GetPixel is for.
    main.push_str("    var p_ret: vec3<f32> = vec3<f32>(0.0);\n");
    main.push_str(&statements);
    // And its last: the alpha comes from the vertex colour, not the preset.
    main.push_str("    return vec4<f32>(p_ret.xyz, in_diffuse.w);\n}\n");

    // The mirrors, declared and filled before anything reads them.
    let mut mirrors = String::new();
    let mut mirror_init = String::new();
    for reg in &e.mirrors {
        mirrors.push_str(&format!("var<private> m_{reg}: vec4<f32>;\n"));
        mirror_init.push_str(&format!("    m_{reg} = u.{reg};\n"));
    }
    let init = format!("{mirror_init}{init}");

    let mut source = String::with_capacity(PRELUDE.len() + main.len() + 4096);
    source.push_str(PRELUDE);
    source.push_str("\n// ---- preset ----\n");
    source.push_str(&mirrors);
    source.push_str(&e.out);
    if needs_init {
        source.push_str("fn milk_init_globals() {\n");
        source.push_str(&init);
        source.push_str("}\n\n");
    }
    source.push_str(&functions);
    source.push_str(&main);

    Ok(Emitted {
        source,
        wants_textures: e.wants_order,
    })
}

/// What a declared name is: its type, and whether it is an array of that.
///
/// Array-ness is not part of [`Ty`], and it has to be here because indexing
/// depends on it -- `samples[i]` is a `float4` when `samples` is an array of
/// them and a `float` when `samples` is one. Reading the second for the first
/// makes `samples[i].w` an error in a file that is perfectly correct.
#[derive(Clone, Copy, Debug)]
struct Binding {
    ty: Ty,
    array: bool,
}

/// A WGSL expression and the HLSL type it has.
#[derive(Clone, Debug)]
struct Value {
    code: String,
    ty: Ty,
    /// True when this names an array rather than a single value, which is
    /// what tells `a[i]` from `v.x` by another spelling.
    array: bool,
    /// True when `code` is a *place* -- a name with accessors on it, which
    /// can be assigned to.
    ///
    /// Carried because WGSL rejects a parenthesised assignment target, and
    /// the obvious defensive habit of wrapping every base in parentheses
    /// turns `ret.x = 1;` into `(p_ret).x = 1;`, which is a syntax error. That
    /// one habit failed every shader in the corpus at once.
    place: bool,
}

impl Value {
    fn expr(code: String, ty: Ty) -> Value {
        Value {
            code,
            ty,
            place: false,
            array: false,
        }
    }

    /// The code, parenthesised only where an accessor could not follow it.
    fn base(&self) -> String {
        if self.place {
            self.code.clone()
        } else {
            format!("({})", self.code)
        }
    }
}

struct Emitter {
    pass: Pass,
    scopes: Vec<HashMap<String, Binding>>,
    globals: HashMap<String, Binding>,
    functions: HashMap<String, (Ty, Vec<Ty>)>,
    /// Module-level declarations, emitted before everything else.
    out: String,
    depth: usize,
    loops: u32,
    temps: u32,
    /// Registers this shader assigns to, which therefore need a mutable copy.
    ///
    /// HLSL's uniforms are writable globals inside a shader -- `q25 = 1;` is
    /// ordinary, and 504 shader bodies in the corpus do it -- while WGSL's
    /// uniform buffer is read-only. Only the registers actually written are
    /// mirrored, so a shader that reads them pays nothing.
    mirrors: BTreeSet<String>,
    wants: BTreeSet<String>,
    wants_order: Vec<String>,
}

fn err(line: u32, construct: &'static str, detail: impl Into<String>) -> HlslError {
    HlslError {
        line,
        construct,
        detail: detail.into(),
    }
}

impl Emitter {
    fn declare(&mut self, name: &str, ty: Ty) {
        self.declare_binding(name, Binding { ty, array: false });
    }

    fn declare_binding(&mut self, name: &str, binding: Binding) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_owned(), binding);
        }
    }

    fn lookup(&self, name: &str) -> Option<Binding> {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.get(name).copied())
            .or_else(|| self.globals.get(name).copied())
    }

    fn pad(&self) -> String {
        "    ".repeat(self.depth)
    }

    // ---- declarations ----------------------------------------------------

    fn global(&mut self, var: &Var, init: &mut String) -> Result<(), HlslError> {
        if var.ty == Ty::Sampler {
            // A sampler global names a texture; it is resolved where it is
            // sampled, not where it is declared.
            self.globals.insert(
                var.name.clone(),
                Binding {
                    ty: Ty::Sampler,
                    array: false,
                },
            );
            return Ok(());
        }
        let wgsl = array_type(var.ty, var.array);
        self.out
            .push_str(&format!("var<private> p_{}: {wgsl};\n", var.name));
        self.globals.insert(
            var.name.clone(),
            Binding {
                ty: var.ty,
                array: var.array.is_some(),
            },
        );
        if let Some(value) = &var.init {
            // Assigned in a generated function rather than as a module-scope
            // initializer, because WGSL requires those to be const-expressions
            // and the corpus initializes globals with `sin(...)`.
            self.scopes.push(HashMap::new());
            let code = self.initializer(value, var.ty, var.array)?;
            self.scopes.pop();
            init.push_str(&format!("    p_{} = {code};\n", var.name));
        }
        Ok(())
    }

    fn function(&mut self, f: &Function) -> Result<String, HlslError> {
        let mut params = Vec::new();
        self.scopes.push(HashMap::new());
        for p in &f.params {
            if p.passing.out || p.passing.inout {
                self.scopes.pop();
                return Err(err(
                    f.line,
                    "out-parameter",
                    format!("`{}` takes `{}` by reference", f.name, p.name),
                ));
            }
            if p.ty == Ty::Sampler {
                self.declare(&p.name, Ty::Sampler);
                self.scopes.pop();
                return Err(err(
                    f.line,
                    "sampler-parameter",
                    format!("`{}` takes a sampler as a parameter", f.name),
                ));
            }
            params.push(format!("p_{}_in: {}", p.name, p.ty.wgsl()));
            self.declare(&p.name, p.ty);
        }
        let saved = std::mem::replace(&mut self.depth, 1);
        let body = self.block_body(&f.body)?;
        self.depth = saved;
        self.scopes.pop();

        let ret = if f.ret == Ty::Void {
            String::new()
        } else {
            format!(" -> {}", f.ret.wgsl())
        };
        // Same reason as the entry point's: HLSL passes by value and lets the
        // body assign to the copy; WGSL's parameters are immutable.
        let copies: String = f
            .params
            .iter()
            .map(|p| format!("    var p_{0}: {1} = p_{0}_in;\n", p.name, p.ty.wgsl()))
            .collect();
        let mut out = format!(
            "fn p_{}({}){ret} {{\n{copies}{body}",
            f.name,
            params.join(", ")
        );
        // WGSL requires every path of a value-returning function to return.
        // HLSL's compiler is looser, and a preset whose last branch falls off
        // the end is a preset that compiled there and would not here.
        if f.ret != Ty::Void && !ends_with_return(&f.body) {
            out.push_str(&format!("    return {};\n", zero_of(f.ret)));
        }
        out.push_str("}\n\n");
        Ok(out)
    }

    // ---- statements ------------------------------------------------------

    fn block_body(&mut self, block: &Block) -> Result<String, HlslError> {
        self.scopes.push(HashMap::new());
        let mut out = String::new();
        for s in &block.0 {
            out.push_str(&self.stmt(s)?);
        }
        self.scopes.pop();
        Ok(out)
    }

    fn stmt(&mut self, s: &Stmt) -> Result<String, HlslError> {
        let pad = self.pad();
        Ok(match s {
            Stmt::Empty => String::new(),
            Stmt::Block(b) => {
                let inner = self.nested(b)?;
                format!("{pad}{{\n{inner}{pad}}}\n")
            }
            Stmt::Decl(vars) => {
                let mut out = String::new();
                for v in vars {
                    let wgsl = array_type(v.ty, v.array);
                    let value = match &v.init {
                        Some(e) => self.initializer(e, v.ty, v.array)?,
                        None => zero_of_array(v.ty, v.array),
                    };
                    out.push_str(&format!("{pad}var p_{}: {wgsl} = {value};\n", v.name));
                    self.declare_binding(
                        &v.name,
                        Binding {
                            ty: v.ty,
                            array: v.array.is_some(),
                        },
                    );
                }
                out
            }
            Stmt::Expr(e) => self.effect(e)?,
            Stmt::Return(None) => format!("{pad}return;\n"),
            Stmt::Return(Some(e)) => {
                let v = self.expr(e)?;
                format!("{pad}return {};\n", v.code)
            }
            Stmt::Break => format!("{pad}break;\n"),
            Stmt::Continue => format!("{pad}continue;\n"),
            Stmt::Discard => format!("{pad}discard;\n"),
            Stmt::If { cond, then, els } => {
                let c = self.expr(cond)?;
                let c = self.as_bool(&c, cond.line)?;
                let a = self.nested(then)?;
                let mut out = format!("{pad}if ({c}) {{\n{a}{pad}}}");
                if let Some(els) = els {
                    let b = self.nested(els)?;
                    out.push_str(&format!(" else {{\n{b}{pad}}}"));
                }
                out.push('\n');
                out
            }
            Stmt::While { cond, body } => {
                let c = self.expr(cond)?;
                let c = self.as_bool(&c, cond.line)?;
                let (guard, tick) = self.guard();
                let inner = self.nested(body)?;
                format!("{pad}{guard}{pad}loop {{\n{tick}{pad}    if (!({c})) {{ break; }}\n{inner}{pad}}}\n")
            }
            Stmt::DoWhile { body, cond } => {
                let (guard, tick) = self.guard();
                let inner = self.nested(body)?;
                let c = self.expr(cond)?;
                let c = self.as_bool(&c, cond.line)?;
                format!("{pad}{guard}{pad}loop {{\n{tick}{inner}{pad}    if (!({c})) {{ break; }}\n{pad}}}\n")
            }
            Stmt::For {
                init,
                cond,
                step,
                body,
            } => {
                // A block of its own, so the loop counter's name does not
                // escape -- and `loop`/`continuing` rather than WGSL's `for`,
                // because a `for` takes one update expression and the corpus
                // writes `i++, j++`.
                self.scopes.push(HashMap::new());
                self.depth += 1;
                let init = match init {
                    Some(s) => self.stmt(s)?,
                    None => String::new(),
                };
                let cond_code = match cond {
                    Some(c) => {
                        let v = self.expr(c)?;
                        self.as_bool(&v, c.line)?
                    }
                    None => "true".to_owned(),
                };
                let inner_pad = self.pad();
                let (guard, tick) = self.guard();
                let body_code = self.nested(body)?;
                let step_code = match step {
                    Some(s) => {
                        self.depth += 1;
                        let c = self.effect(s)?;
                        self.depth -= 1;
                        c
                    }
                    None => String::new(),
                };
                self.depth -= 1;
                self.scopes.pop();
                format!(
                    "{pad}{{\n{init}{inner_pad}{guard}{inner_pad}loop {{\n{tick}\
                     {inner_pad}    if (!({cond_code})) {{ break; }}\n{body_code}\
                     {inner_pad}    continuing {{\n{step_code}{inner_pad}    }}\n{inner_pad}}}\n{pad}}}\n"
                )
            }
        })
    }

    fn nested(&mut self, block: &Block) -> Result<String, HlslError> {
        self.depth += 1;
        let out = self.block_body(block);
        self.depth -= 1;
        out
    }

    /// A loop's runaway guard: the declaration, and the line that ticks it.
    fn guard(&mut self) -> (String, String) {
        self.loops += 1;
        let n = self.loops;
        let pad = self.pad();
        (
            format!("var milk_guard{n}: i32 = 0;\n"),
            format!("{pad}    milk_guard{n} += 1; if (milk_guard{n} > {MAX_LOOP}) {{ break; }}\n"),
        )
    }

    /// An expression evaluated for its effect: an assignment, a call, an
    /// increment, or a comma-sequence of those.
    fn effect(&mut self, e: &Expr) -> Result<String, HlslError> {
        let pad = self.pad();
        Ok(match &e.kind {
            ExprKind::Sequence(items) => {
                let mut out = String::new();
                for item in items {
                    out.push_str(&self.effect(item)?);
                }
                out
            }
            ExprKind::Assign { op, target, value } => {
                let t = self.expr(target)?;
                let v = self.expr(value)?;
                let fitted = match op {
                    None => self.fit(&v, t.ty, e.line)?,
                    Some(op) => {
                        // Spelled out rather than using WGSL's `+=`, because
                        // HLSL's compound assignment truncates the right side
                        // to the target's shape and WGSL's does not.
                        let combined = self.binary(*op, &t, &v, e.line)?;
                        self.fit(&combined, t.ty, e.line)?
                    }
                };
                self.assign(&t, fitted, e.line)?
            }
            ExprKind::PostIncDec { expr, inc } => {
                let t = self.expr(expr)?;
                let one = if t.ty.base() == Some(Base::Int) {
                    "1"
                } else {
                    "1.0"
                };
                let op = if *inc { "+" } else { "-" };
                format!("{pad}{0} = {0} {op} {one};\n", t.code)
            }
            ExprKind::Unary {
                op: UnOp::PreInc,
                expr,
            }
            | ExprKind::Unary {
                op: UnOp::PreDec,
                expr,
            } => {
                let inc = matches!(
                    e.kind,
                    ExprKind::Unary {
                        op: UnOp::PreInc,
                        ..
                    }
                );
                let t = self.expr(expr)?;
                let one = if t.ty.base() == Some(Base::Int) {
                    "1"
                } else {
                    "1.0"
                };
                let op = if inc { "+" } else { "-" };
                format!("{pad}{0} = {0} {op} {one};\n", t.code)
            }
            ExprKind::Call { .. } => {
                let v = self.expr(e)?;
                if v.ty == Ty::Void {
                    format!("{pad}{};\n", v.code)
                } else {
                    // WGSL has no expression statements that discard a value.
                    format!("{pad}_ = {};\n", v.code)
                }
            }
            // `texsize.zw*r;` on a line of its own -- HLSL warns and moves
            // on, and so does this. WGSL's phony assignment keeps any call
            // inside it while discarding the value.
            _ => {
                let v = self.expr(e)?;
                format!("{pad}_ = {};\n", v.code)
            }
        })
    }

    /// Write one assignment, splitting a swizzled target into its lanes.
    ///
    /// **WGSL has no assignment to a swizzle at all** -- not even `v.xy = a`,
    /// which every other shading language accepts. Presets write it constantly
    /// (`uv.xy = ...`, `ret.xy = ...`), so it is decomposed: the value is
    /// bound once, and each lane assigned from it. Binding it first matters --
    /// the right-hand side may read the very components being written, and
    /// `v.xy = v.yx` is a swap rather than a duplication.
    fn assign(&mut self, place: &Value, value: String, line: u32) -> Result<String, HlslError> {
        let pad = self.pad();
        if !place.place {
            return Err(err(
                line,
                "assignment-target",
                "this expression cannot be assigned to",
            ));
        }
        // The test is on the *emitted* place rather than on the tree, because
        // the commonest swizzled target in the corpus does not look like one
        // in the tree: `uv` is a plain name that resolves to `in_uv.xy`.
        let Some((base, lanes)) = split_swizzle(&place.code) else {
            return Ok(format!("{pad}{} = {value};\n", place.code));
        };
        self.temps += 1;
        let temp = format!("milk_t{}", self.temps);
        let mut out = format!("{pad}let {temp} = {value};\n");
        for (i, lane) in lanes.chars().enumerate() {
            out.push_str(&format!(
                "{pad}{base}.{lane} = {temp}.{};\n",
                &"xyzw"[i..i + 1]
            ));
        }
        Ok(out)
    }

    // ---- expressions -----------------------------------------------------

    fn expr(&mut self, e: &Expr) -> Result<Value, HlslError> {
        match &e.kind {
            ExprKind::Number { value, is_int } => Ok(Value::expr(
                if *is_int {
                    format!("{}", *value as i64)
                } else {
                    format_float(*value)
                },
                if *is_int {
                    Ty::Vec {
                        base: Base::Int,
                        n: 1,
                    }
                } else {
                    Ty::FLOAT
                },
            )),
            ExprKind::Bool(b) => Ok(Value::expr(b.to_string(), Ty::BOOL)),
            ExprKind::Name(name) => self.name(name, e.line),
            ExprKind::Member { base, name } => {
                let b = self.expr(base)?;
                let ty = ty::swizzle(b.ty, name).ok_or_else(|| {
                    err(
                        e.line,
                        "swizzle",
                        format!("`.{name}` is not a component of a `{}`", b.ty),
                    )
                })?;
                // HLSL lets a scalar be swizzled -- `x.x` is `x` and `x.xxx`
                // is a grey -- and WGSL has no component accessors on `f32`
                // at all. So a scalar swizzle becomes the value or a splat of
                // it, which is what it meant.
                if b.ty.is_scalar() {
                    return Ok(if ty.is_scalar() {
                        b
                    } else {
                        Value::expr(format!("{}({})", ty.wgsl(), b.code), ty)
                    });
                }
                // A swizzle *of* a swizzle is composed rather than chained.
                // `uv` resolves to `in_uv.xy`, so `uv.x` would otherwise emit
                // `in_uv.xy.x` -- which WGSL will read but never assign to,
                // and `uv.x = ...` is an ordinary line in these presets.
                let (code, place) = match (b.place, split_swizzle(&b.code)) {
                    (true, Some((root, lanes))) => (
                        format!("{root}.{}", compose_swizzle(lanes, name)),
                        is_simple_swizzle(name),
                    ),
                    _ => (
                        format!("{}.{name}", b.base()),
                        // A swizzle naming each lane once is still
                        // assignable; `ret.x = 1` is ordinary and
                        // `ret.xx = 1` is not.
                        b.place && is_simple_swizzle(name),
                    ),
                };
                Ok(Value {
                    code,
                    ty,
                    place,
                    array: false,
                })
            }
            ExprKind::Index { base, index } => {
                let b = self.expr(base)?;
                let i = self.expr(index)?;
                let i = self.fit(
                    &i,
                    Ty::Vec {
                        base: Base::Int,
                        n: 1,
                    },
                    e.line,
                )?;
                // Indexing a vector yields its base; indexing an array yields
                // the element type, which is the declared type either way,
                // because `ty` does not carry array-ness.
                let ty = if b.array {
                    // An array of `T` indexes to a `T`, whatever `T` is.
                    b.ty
                } else {
                    match b.ty {
                        Ty::Vec { base, n } if n > 1 => Ty::Vec { base, n: 1 },
                        // HLSL indexes a matrix by row, which is WGSL's column
                        // under this mapping -- so `m[0]` is the same vector in
                        // both, of `cols` components.
                        Ty::Mat { base, cols, .. } => Ty::Vec { base, n: cols },
                        other => other,
                    }
                };
                Ok(Value {
                    code: format!("{}[{i}]", b.base()),
                    ty,
                    place: b.place,
                    array: false,
                })
            }
            ExprKind::Unary { op, expr } => {
                let v = self.expr(expr)?;
                match op {
                    UnOp::Plus => Ok(v),
                    UnOp::Neg => {
                        // `-(a < b)` -- HLSL negates a comparison by promoting
                        // it first, and WGSL cannot negate a `bool` at all.
                        let ty = if v.ty.base() == Some(Base::Bool) {
                            v.ty.with_base(Base::Float)
                        } else {
                            v.ty
                        };
                        let code = self.fit(&v, ty, e.line)?;
                        Ok(Value::expr(format!("-({code})"), ty))
                    }
                    UnOp::Not => {
                        let b = self.as_bool(&v, e.line)?;
                        Ok(Value::expr(format!("!({b})"), Ty::BOOL))
                    }
                    UnOp::PreInc | UnOp::PreDec => Err(err(
                        e.line,
                        "increment-in-expression",
                        "`++` and `--` are only statements here, not values",
                    )),
                }
            }
            ExprKind::PostIncDec { .. } => Err(err(
                e.line,
                "increment-in-expression",
                "`++` and `--` are only statements here, not values",
            )),
            ExprKind::Binary { op, lhs, rhs } => {
                let a = self.expr(lhs)?;
                let b = self.expr(rhs)?;
                self.binary(*op, &a, &b, e.line)
            }
            ExprKind::Assign { .. } => Err(err(
                e.line,
                "assignment-in-expression",
                "an assignment is only a statement here, not a value",
            )),
            ExprKind::Ternary { cond, then, els } => {
                let c = self.expr(cond)?;
                let a = self.expr(then)?;
                let b = self.expr(els)?;
                let ty = Ty::binary(a.ty, b.ty).ok_or_else(|| {
                    err(
                        e.line,
                        "ternary",
                        format!("`{}` and `{}` have no common type", a.ty, b.ty),
                    )
                })?;
                let a = self.fit(&a, ty, e.line)?;
                let b = self.fit(&b, ty, e.line)?;
                let c = self.as_bool(&c, e.line)?;
                // `select(false_value, true_value, condition)` -- the operands
                // are the other way round from `?:`, which is a silent bug if
                // it is ever written the intuitive way.
                Ok(Value::expr(format!("select({b}, {a}, {c})"), ty))
            }
            ExprKind::Sequence(items) => {
                // C's comma operator evaluates each and takes the last, and
                // WGSL has no such operator. Where everything before the last
                // is pure -- which is the only form the corpus writes,
                // `texsize.zx*(q3,q3)` -- the earlier ones can simply be
                // dropped, because evaluating them changes nothing.
                let Some(last) = items.last() else {
                    return Err(err(e.line, "comma-in-expression", "an empty expression"));
                };
                if !items[..items.len() - 1].iter().all(is_pure) {
                    return Err(err(
                        e.line,
                        "comma-in-expression",
                        "a comma expression with side effects is only a statement here",
                    ));
                }
                self.expr(last)
            }
            ExprKind::InitList(_) => Err(err(
                e.line,
                "initializer",
                "an initializer list only belongs in a declaration",
            )),
            ExprKind::Call { name, args } => self.call(name, args, e.line),
        }
    }

    fn name(&mut self, name: &str, line: u32) -> Result<Value, HlslError> {
        if let Some(binding) = self.lookup(name) {
            if binding.ty == Ty::Sampler {
                return Err(err(
                    line,
                    "sampler-as-value",
                    format!("`{name}` is a sampler and cannot be used as a value"),
                ));
            }
            return Ok(Value {
                code: format!("p_{name}"),
                ty: binding.ty,
                place: true,
                array: binding.array,
            });
        }
        if let Some(alias) = env::lookup(self.pass, name) {
            // The entry point's own inputs are assignable -- `uv = 0.05 +
            // 0.95*uv;` is how a preset insets its picture, and it is one of
            // the commonest lines in the corpus. A uniform is not: `time` is
            // read-only in HLSL too.
            let is_input = alias.target.starts_with("_uv")
                || alias.target.starts_with("_rad_ang")
                || alias.target.starts_with("_vDiffuse");
            let (head, suffix) = match alias.target.split_once('.') {
                Some((h, t)) => (h, format!(".{t}")),
                None => (alias.target, String::new()),
            };
            let mirrored = head
                .strip_prefix('_')
                .is_some_and(|reg| self.mirrors.contains(reg));
            return Ok(Value {
                code: if mirrored {
                    format!("m_{}{suffix}", &head[1..])
                } else {
                    alias_code(alias.target)
                },
                ty: alias.ty,
                place: is_input || mirrored,
                array: false,
            });
        }
        if let Some((_, value)) = env::CONSTANTS.iter().find(|(n, _)| *n == name) {
            return Ok(Value::expr(format_float(*value), Ty::FLOAT));
        }
        if let Some(ty) = env::register(name) {
            let reg = &name[1..];
            if self.mirrors.contains(reg) {
                return Ok(Value {
                    code: format!("m_{reg}"),
                    ty,
                    place: true,
                    array: false,
                });
            }
            return Ok(Value::expr(format!("u.{reg}"), ty));
        }
        if env::MATRICES.contains(&name) {
            return Ok(Value::expr(
                format!("u.{name}"),
                Ty::Mat {
                    base: Base::Float,
                    rows: 4,
                    cols: 3,
                },
            ));
        }
        if name.starts_with("sampler_") {
            return Err(err(
                line,
                "sampler-as-value",
                format!("`{name}` is a sampler and cannot be used as a value"),
            ));
        }
        Err(err(
            line,
            "unknown-name",
            format!("`{name}` is not declared and is not part of MilkDrop's shader environment"),
        ))
    }

    fn binary(&mut self, op: BinOp, a: &Value, b: &Value, line: u32) -> Result<Value, HlslError> {
        if op.is_logical() {
            let x = self.as_bool(a, line)?;
            let y = self.as_bool(b, line)?;
            let sym = if op == BinOp::And { "&&" } else { "||" };
            return Ok(Value::expr(format!("({x} {sym} {y})"), Ty::BOOL));
        }
        let mut common = Ty::binary(a.ty, b.ty).ok_or_else(|| {
            err(
                line,
                "operand-types",
                format!("`{}` and `{}` have no common type", a.ty, b.ty),
            )
        })?;
        // `(a >= b) * (c < d)` -- HLSL promotes a bool to a number the moment
        // it is used as one, and WGSL has no arithmetic on `bool` at all. The
        // corpus multiplies comparisons together as a branch-free `and`.
        if common.base() == Some(Base::Bool) {
            common = common.with_base(Base::Float);
        }
        // A matrix operand means this is `mul`-shaped arithmetic rather than
        // componentwise, and HLSL's `*` on two matrices is componentwise while
        // WGSL's is not. Refused rather than guessed.
        if matches!(a.ty, Ty::Mat { .. }) || matches!(b.ty, Ty::Mat { .. }) {
            if !a.ty.is_scalar() && !b.ty.is_scalar() {
                return Err(err(
                    line,
                    "matrix-operator",
                    "an operator on two matrices means different things in HLSL and WGSL",
                ));
            }
            // WGSL defines only `*` and `/` between a matrix and a scalar.
            // HLSL broadcasts every operator, so `m - 0.01` is ordinary there
            // and has no form here -- the scalar is built into a matrix.
            let sym = symbol(op);
            let (x, y) = match (op, a.ty.is_scalar(), b.ty.is_scalar()) {
                (BinOp::Mul, _, _) => (a.code.clone(), b.code.clone()),
                // `m / s` is `m * (1/s)`, which WGSL does define.
                (BinOp::Div, false, true) => {
                    return Ok(Value::expr(
                        format!("({} * (1.0 / {}))", a.code, b.code),
                        common,
                    ))
                }
                (_, true, false) => (splat_matrix(&a.code, b.ty), b.code.clone()),
                (_, false, true) => (a.code.clone(), splat_matrix(&b.code, a.ty)),
                _ => (a.code.clone(), b.code.clone()),
            };
            return Ok(Value::expr(format!("({x} {sym} {y})"), common));
        }

        let x = self.fit(a, common, line)?;
        let y = self.fit(b, common, line)?;
        let sym = symbol(op);
        let ty = if op.is_comparison() {
            Ty::Vec {
                base: Base::Bool,
                n: common.components(),
            }
        } else {
            common
        };
        Ok(Value::expr(format!("({x} {sym} {y})"), ty))
    }

    fn call(&mut self, name: &str, args: &[Expr], line: u32) -> Result<Value, HlslError> {
        // A constructor: `float2(x, y)`, `float3(v)`.
        if let Some(target) = Ty::from_name(name) {
            return self.constructor(target, args, line);
        }
        // A texture read, which needs its sampler argument resolved to a
        // binding rather than to a value.
        if matches!(
            name,
            "tex2D" | "tex2d" | "tex3D" | "tex3d" | "tex2Dlod" | "tex2Dbias" | "tex3Dlod"
        ) {
            return self.texture(name, args, line);
        }
        if name == "mul" {
            return self.mul(args, line);
        }
        // One the preset declared itself.
        if let Some((ret, params)) = self.functions.get(name).cloned() {
            if args.len() != params.len() {
                return Err(err(
                    line,
                    "arity",
                    format!(
                        "`{name}` takes {} argument(s), not {}",
                        params.len(),
                        args.len()
                    ),
                ));
            }
            let mut out = Vec::new();
            for (a, want) in args.iter().zip(params) {
                let v = self.expr(a)?;
                out.push(self.fit(&v, want, line)?);
            }
            return Ok(Value::expr(format!("p_{name}({})", out.join(", ")), ret));
        }
        // `all` and `any` answer a *bool*, and take one in WGSL. HLSL takes
        // anything and tests it against zero, so the conversion is written
        // out -- typed as a number instead, `all(x)` emits `all(x) != 0.0`,
        // which compares a bool to a float.
        if matches!(name, "all" | "any") && args.len() == 1 {
            let v = self.expr(&args[0])?;
            let inner = self.as_bool_vector(&v, line)?;
            return Ok(Value::expr(format!("{name}({inner})"), Ty::BOOL));
        }
        // `normalize` of a scalar is its sign. HLSL accepts it; WGSL wants a
        // vector, and the two agree on what the answer is.
        if name == "normalize" && args.len() == 1 {
            let v = self.expr(&args[0])?;
            if v.ty.is_scalar() {
                let fitted = self.fit(&v, Ty::FLOAT, line)?;
                return Ok(Value::expr(format!("sign({fitted})"), Ty::FLOAT));
            }
        }
        // `lum` is a macro over `dot`, so it inherits `dot`'s truncation: a
        // `float2` argument is perfectly legal and weighs only two channels.
        // A fixed `float3` signature refuses `lum(roam_sin.xy)`, which the
        // corpus writes.
        if name == "lum" {
            if args.len() != 1 {
                return Err(err(line, "arity", "`lum` takes one argument"));
            }
            let v = self.expr(&args[0])?;
            let n = v.ty.components().clamp(1, 3);
            const WEIGHTS: [&str; 3] = ["0.32", "0.49", "0.29"];
            if n == 1 {
                let fitted = self.fit(&v, Ty::FLOAT, line)?;
                return Ok(Value::expr(format!("({fitted} * 0.32)"), Ty::FLOAT));
            }
            let want = Ty::Vec {
                base: Base::Float,
                n,
            };
            let fitted = self.fit(&v, want, line)?;
            return Ok(Value::expr(
                format!(
                    "dot({fitted}, {}({}))",
                    want.wgsl(),
                    WEIGHTS[..n as usize].join(", ")
                ),
                Ty::FLOAT,
            ));
        }
        // MilkDrop's own helpers, which are functions in the prelude.
        if let Some((wgsl, arg, ret)) = env_helper(name) {
            if args.len() != 1 {
                return Err(err(
                    line,
                    "arity",
                    format!("`{name}` takes one argument, not {}", args.len()),
                ));
            }
            let v = self.expr(&args[0])?;
            let fitted = self.fit(&v, arg, line)?;
            return Ok(Value::expr(format!("{wgsl}({fitted})"), ret));
        }
        let Some(b) = builtin(name) else {
            return Err(err(
                line,
                "unknown-function",
                format!("`{name}` is not a function this build knows"),
            ));
        };
        let mut values = Vec::new();
        for a in args {
            values.push(self.expr(a)?);
        }
        if values.is_empty() {
            return Err(err(
                line,
                "arity",
                format!("`{name}` was called with no arguments"),
            ));
        }
        let (code, ty) = match b.ret {
            Ret::Arg0 => {
                let ty = if values[0].ty.base() == Some(Base::Bool) {
                    values[0].ty.with_base(Base::Float)
                } else {
                    values[0].ty
                };
                let mut parts = Vec::new();
                for v in &values {
                    parts.push(self.fit(v, ty, line)?);
                }
                (format!("{}({})", b.wgsl, parts.join(", ")), ty)
            }
            Ret::Scalar => {
                let parts: Vec<String> = values.into_iter().map(|v| v.code).collect();
                (format!("{}({})", b.wgsl, parts.join(", ")), Ty::FLOAT)
            }
            Ret::Common | Ret::CommonScalar | Ret::Vec3 => {
                let mut common = values[0].ty;
                for v in &values[1..] {
                    common = Ty::binary(common, v.ty).ok_or_else(|| {
                        err(
                            line,
                            "operand-types",
                            format!("`{name}`'s arguments have no common type"),
                        )
                    })?;
                }
                // Same promotion as arithmetic: a comparison handed to `pow`
                // or `max` is a number by the time it gets there.
                if common.base() == Some(Base::Bool) {
                    common = common.with_base(Base::Float);
                }
                let mut parts = Vec::new();
                for v in &values {
                    parts.push(self.fit(v, common, line)?);
                }
                let ty = match b.ret {
                    Ret::CommonScalar => Ty::Vec {
                        base: common.base().unwrap_or(Base::Float),
                        n: 1,
                    },
                    Ret::Vec3 => Ty::FLOAT3,
                    _ => common,
                };
                (format!("{}({})", b.wgsl, parts.join(", ")), ty)
            }
        };
        // `fmod` is an operator in WGSL rather than a function, and its `%`
        // on floats is HLSL's fmod exactly: the remainder takes the sign of
        // the dividend.
        if name == "fmod" {
            let inner = code
                .strip_prefix("__fmod(")
                .and_then(|s| s.strip_suffix(')'))
                .unwrap_or_default();
            let (a, rest) = inner.split_once(", ").unwrap_or((inner, "0.0"));
            return Ok(Value::expr(format!("({a} % {rest})"), ty));
        }
        Ok(Value::expr(code, ty))
    }

    fn constructor(&mut self, target: Ty, args: &[Expr], line: u32) -> Result<Value, HlslError> {
        let mut values = Vec::new();
        for a in args {
            values.push(self.expr(a)?);
        }
        let base = target.base().unwrap_or(Base::Float);
        // A matrix is built from its columns. WGSL takes nothing else --
        // neither `mat2x2<f32>(a,b,c,d)` nor `mat2x2<f32>(v)` -- while HLSL
        // takes both, and `float2x2(_qb)` from a `float4` is in the corpus.
        // So every argument is flattened to scalars and regrouped, which
        // covers both spellings and their mixtures.
        if let Ty::Mat { rows, cols, .. } = target {
            let mut flat: Vec<Value> = Vec::new();
            for v in &values {
                let n = v.ty.components();
                if n <= 1 {
                    flat.push(v.clone());
                } else {
                    for c in 0..n as usize {
                        flat.push(Value::expr(
                            format!("{}.{}", v.base(), &"xyzw"[c..c + 1]),
                            Ty::Vec { base, n: 1 },
                        ));
                    }
                }
            }
            if flat.len() != (rows * cols) as usize {
                return Err(err(
                    line,
                    "constructor",
                    format!(
                        "{} value(s) for a `{target}`, which needs {}",
                        flat.len(),
                        rows * cols
                    ),
                ));
            }
            let want = Ty::Vec { base, n: cols };
            let mut columns = Vec::new();
            for chunk in flat.chunks(cols as usize) {
                let mut parts = Vec::new();
                for v in chunk {
                    parts.push(self.fit(v, Ty::Vec { base, n: 1 }, line)?);
                }
                columns.push(format!("{}({})", want.wgsl(), parts.join(", ")));
            }
            return Ok(Value::expr(
                format!("{}({})", target.wgsl(), columns.join(", ")),
                target,
            ));
        }
        // `float3(v)` with one argument of the wrong width is HLSL's splat or
        // truncation; with several it is a concatenation, and WGSL accepts a
        // mixture of scalars and vectors as long as the components add up.
        if values.len() == 1 {
            let fitted = self.fit(&values[0], target, line)?;
            return Ok(Value::expr(fitted, target));
        }
        let mut parts = Vec::new();
        for v in &values {
            let want = Ty::Vec {
                base,
                n: v.ty.components().max(1),
            };
            parts.push(self.fit(v, want, line)?);
        }
        Ok(Value::expr(
            format!("{}({})", target.wgsl(), parts.join(", ")),
            target,
        ))
    }

    fn texture(&mut self, name: &str, args: &[Expr], line: u32) -> Result<Value, HlslError> {
        if args.len() < 2 {
            return Err(err(
                line,
                "arity",
                format!("`{name}` takes a sampler and a coordinate"),
            ));
        }
        let ExprKind::Name(sampler) = &args[0].kind else {
            return Err(err(
                line,
                "sampler-expression",
                format!("`{name}`'s first argument has to name a sampler"),
            ));
        };
        let three = name.eq_ignore_ascii_case("tex3d");
        let (texture, samp, is_3d) = self.binding_for(sampler);
        if is_3d != three {
            // `tex2D` of a volume, or `tex3D` of a plane. HLSL would refuse
            // it and so does this, rather than sampling the wrong thing.
            return Err(err(
                line,
                "sampler-dimensions",
                format!("`{name}` cannot read `{sampler}`"),
            ));
        }
        let coord = self.expr(&args[1])?;
        // `tex2Dlod` and `tex2Dbias` carry the level in the coordinate's spare
        // lanes. Every read here is at level zero already -- the feedback and
        // blur targets have one -- so the extra lanes are dropped rather than
        // honoured, which is the same picture on a single-level texture.
        let want = if three { Ty::FLOAT3 } else { Ty::FLOAT2 };
        let coord = self.fit(&coord, want, line)?;
        let call = if three { "milk_tex3D" } else { "milk_tex2D" };
        Ok(Value::expr(
            format!("{call}({texture}, {samp}, {coord})"),
            Ty::FLOAT4,
        ))
    }

    /// The texture and sampler bindings a MilkDrop sampler name resolves to.
    ///
    /// The naming is `sampler_[fp][cw]_<what>`, with the filter/address pair
    /// optional and `sampler_main` meaning `sampler_fw_main`. A `<what>` this
    /// build does not have is a texture pack (decision 56): it reads the
    /// placeholder, and the name is reported.
    fn binding_for(&mut self, name: &str) -> (&'static str, &'static str, bool) {
        let rest = name.strip_prefix("sampler_").unwrap_or(name);
        let (samp, what) = match rest.split_once('_') {
            Some(("fw", w)) => ("samp_fw", w),
            Some(("fc", w)) => ("samp_fc", w),
            Some(("pw", w)) => ("samp_pw", w),
            Some(("pc", w)) => ("samp_pc", w),
            _ => ("samp_fw", rest),
        };
        match what {
            "main" => ("tex_main", samp, false),
            "blur1" => ("tex_blur1", samp, false),
            "blur2" => ("tex_blur2", samp, false),
            "blur3" => ("tex_blur3", samp, false),
            "noise_lq" => ("tex_noise_lq", samp, false),
            "noise_lq_lite" => ("tex_noise_lq_lite", samp, false),
            "noise_mq" => ("tex_noise_mq", samp, false),
            "noise_hq" => ("tex_noise_hq", samp, false),
            "noisevol_lq" => ("tex_noisevol_lq", samp, true),
            "noisevol_hq" => ("tex_noisevol_hq", samp, true),
            other => {
                if self.wants.insert(other.to_owned()) {
                    self.wants_order.push(other.to_owned());
                }
                ("tex_placeholder", samp, false)
            }
        }
    }

    fn mul(&mut self, args: &[Expr], line: u32) -> Result<Value, HlslError> {
        if args.len() != 2 {
            return Err(err(line, "arity", "`mul` takes two arguments"));
        }
        let a = self.expr(&args[0])?;
        let b = self.expr(&args[1])?;
        // `mul(a, b)` becomes `b * a`, in every combination.
        //
        // `ty.rs` takes HLSL's rows to be WGSL's columns, which makes
        // construction and indexing agree exactly and leaves this as the one
        // place that pays for it. HLSL's `mul(v, H)` is a row vector times a
        // matrix; with H stored column-for-row, the same arithmetic is WGSL's
        // `H * v`. The matrix-first and matrix-times-matrix cases flip for the
        // same reason.
        let ty = match (a.ty, b.ty) {
            (Ty::Vec { n, .. }, Ty::Mat { rows, cols, base }) if n == rows => {
                Ty::Vec { base, n: cols }
            }
            (Ty::Mat { rows, cols, base }, Ty::Vec { n, .. }) if n == cols => {
                Ty::Vec { base, n: rows }
            }
            (
                Ty::Mat {
                    rows, cols: mid, ..
                },
                Ty::Mat {
                    rows: mid2,
                    cols,
                    base,
                },
            ) if mid == mid2 => Ty::Mat { base, rows, cols },
            // Neither side is a matrix, so `mul` is an ordinary product --
            // and its operands need the same fitting any other product's do.
            // Without it `mul(uv1, _qb)` emits `vec4 * vec2`, which HLSL
            // truncates and WGSL refuses.
            (x, y) => {
                let common = Ty::binary(x, y).ok_or_else(|| {
                    err(
                        line,
                        "operand-types",
                        format!("`mul` cannot combine a `{x}` and a `{y}`"),
                    )
                })?;
                let x = self.fit(&a, common, line)?;
                let y = self.fit(&b, common, line)?;
                return Ok(Value::expr(format!("({x} * {y})"), common));
            }
        };
        Ok(Value::expr(format!("({} * {})", b.code, a.code), ty))
    }

    // ---- fitting ---------------------------------------------------------

    /// Reshape a value into the type its context wants, the way HLSL would
    /// have done silently.
    fn fit(&mut self, v: &Value, want: Ty, line: u32) -> Result<String, HlslError> {
        let fit = v.ty.fit_to(want).ok_or_else(|| {
            err(
                line,
                "conversion",
                format!("a `{}` cannot become a `{want}`", v.ty),
            )
        })?;
        let mut code = v.code.clone();
        // Base first, on the source's own shape, then the shape.
        if let Some(base) = fit.base {
            let as_base = v.ty.with_base(base);
            code = format!("{}({code})", as_base.wgsl());
        }
        let simple = fit.base.is_none() && v.place;
        let base = if simple {
            code.clone()
        } else {
            format!("({code})")
        };
        Ok(match fit.shape {
            Shape::Same => code,
            Shape::Splat => format!("{}({code})", want.wgsl()),
            Shape::Truncate(n) => format!("{base}.{}", &"xyzw"[..n as usize]),
        })
    }

    /// A `bool` of the same width as the value, for `all` and `any`.
    fn as_bool_vector(&mut self, v: &Value, line: u32) -> Result<String, HlslError> {
        match v.ty {
            Ty::Vec {
                base: Base::Bool, ..
            } => Ok(v.code.clone()),
            Ty::Vec { base, n: 1 } => {
                let zero = if base == Base::Int { "0" } else { "0.0" };
                Ok(format!("({} != {zero})", v.code))
            }
            Ty::Vec { base, n } => {
                let ty = Ty::Vec { base, n };
                let inner = if base == Base::Int { "0" } else { "0.0" };
                Ok(format!("({} != {}({inner}))", v.code, ty.wgsl()))
            }
            other => Err(err(
                line,
                "condition",
                format!("a `{other}` cannot be tested"),
            )),
        }
    }

    /// A scalar `bool`, which is what WGSL's `if`, `&&` and `select` require.
    ///
    /// HLSL's truthiness is "not zero", and its conditions may be numeric.
    fn as_bool(&mut self, v: &Value, line: u32) -> Result<String, HlslError> {
        match v.ty {
            Ty::Vec {
                base: Base::Bool,
                n: 1,
            } => Ok(v.code.clone()),
            Ty::Vec {
                base: Base::Bool, ..
            } => Ok(format!("all({})", v.code)),
            Ty::Vec { base, n: 1 } => {
                let zero = if base == Base::Int { "0" } else { "0.0" };
                Ok(format!("({} != {zero})", v.code))
            }
            Ty::Vec { base, n } => {
                let zero = Ty::Vec { base, n }.wgsl();
                let inner = if base == Base::Int { "0" } else { "0.0" };
                Ok(format!("any({} != {zero}({inner}))", v.code))
            }
            other => Err(err(
                line,
                "condition",
                format!("a `{other}` is not a condition"),
            )),
        }
    }

    /// A declaration's initializer, which may be an aggregate.
    fn initializer(&mut self, e: &Expr, ty: Ty, array: Option<u32>) -> Result<String, HlslError> {
        let ExprKind::InitList(items) = &e.kind else {
            let v = self.expr(e)?;
            return match array {
                None => self.fit(&v, ty, e.line),
                Some(_) => Err(err(
                    e.line,
                    "initializer",
                    "an array needs a `{ ... }` initializer",
                )),
            };
        };

        // HLSL flattens aggregates: `float2 o[10] = { a, b, c, d, ... }` is
        // ten pairs written as twenty scalars, and the corpus writes exactly
        // that. So the components are collected and regrouped by the target's
        // shape rather than by the writer's braces.
        let mut flat: Vec<Value> = Vec::new();
        self.flatten(items, &mut flat)?;

        let per = ty.components().max(1) as usize;
        let want_groups = array.unwrap_or(1) as usize;
        if flat.len() != per * want_groups {
            return Err(err(
                e.line,
                "initializer",
                format!(
                    "{} value(s) for a type that needs {}",
                    flat.len(),
                    per * want_groups
                ),
            ));
        }
        let base = ty.base().unwrap_or(Base::Float);
        let scalar = Ty::Vec { base, n: 1 };
        let mut groups = Vec::new();
        for chunk in flat.chunks(per) {
            let mut parts = Vec::new();
            for v in chunk {
                parts.push(self.fit(v, scalar, e.line)?);
            }
            groups.push(if per == 1 {
                parts.remove(0)
            } else {
                format!("{}({})", ty.wgsl(), parts.join(", "))
            });
        }
        Ok(match array {
            None => groups.remove(0),
            Some(n) => format!("array<{}, {n}>({})", ty.wgsl(), groups.join(", ")),
        })
    }

    fn flatten(&mut self, items: &[Expr], out: &mut Vec<Value>) -> Result<(), HlslError> {
        for item in items {
            match &item.kind {
                ExprKind::InitList(inner) => self.flatten(inner, out)?,
                _ => {
                    let v = self.expr(item)?;
                    // A vector inside an initializer contributes its lanes.
                    let n = v.ty.components();
                    if n <= 1 {
                        out.push(v);
                    } else {
                        for c in 0..n as usize {
                            out.push(Value::expr(
                                format!("{}.{}", v.base(), &"xyzw"[c..c + 1]),
                                Ty::Vec {
                                    base: v.ty.base().unwrap_or(Base::Float),
                                    n: 1,
                                },
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// How a builtin's result type is worked out.
#[derive(Clone, Copy)]
enum Ret {
    /// The type of the first argument, others passed through.
    Arg0,
    /// All arguments fitted to their common type, which is the result.
    Common,
    /// All arguments fitted to their common type; the result is one scalar.
    CommonScalar,
    /// A float, whatever went in.
    Scalar,
    Vec3,
}

struct Builtin {
    wgsl: &'static str,
    ret: Ret,
}

/// The HLSL functions this build knows, and their WGSL spellings.
///
/// Drawn from the corpus's own ranked call list rather than from HLSL's
/// reference: everything a real collection uses, and nothing invented. A name
/// not here is reported as an unknown function against the preset, with its
/// line.
fn builtin(name: &str) -> Option<Builtin> {
    /// `(what a preset writes, what WGSL calls it, how it types)`.
    ///
    /// A table rather than a `match` so that every WGSL spelling is a
    /// `'static` string -- the ones that share HLSL's name are still written
    /// out, because "the same name" is a fact about today's WGSL rather than
    /// a rule.
    const TABLE: &[(&str, &str, Ret)] = &[
        ("abs", "abs", Ret::Arg0),
        ("sin", "sin", Ret::Arg0),
        ("cos", "cos", Ret::Arg0),
        ("tan", "tan", Ret::Arg0),
        ("asin", "asin", Ret::Arg0),
        ("acos", "acos", Ret::Arg0),
        ("atan", "atan", Ret::Arg0),
        ("sinh", "sinh", Ret::Arg0),
        ("cosh", "cosh", Ret::Arg0),
        ("tanh", "tanh", Ret::Arg0),
        ("exp", "exp", Ret::Arg0),
        ("exp2", "exp2", Ret::Arg0),
        ("log", "log", Ret::Arg0),
        ("log2", "log2", Ret::Arg0),
        ("sqrt", "sqrt", Ret::Arg0),
        ("floor", "floor", Ret::Arg0),
        ("ceil", "ceil", Ret::Arg0),
        ("round", "round", Ret::Arg0),
        ("trunc", "trunc", Ret::Arg0),
        ("sign", "sign", Ret::Arg0),
        ("normalize", "normalize", Ret::Arg0),
        ("saturate", "saturate", Ret::Arg0),
        ("fwidth", "fwidth", Ret::Arg0),
        ("frac", "fract", Ret::Arg0),
        ("rsqrt", "inverseSqrt", Ret::Arg0),
        ("ddx", "dpdx", Ret::Arg0),
        ("ddy", "dpdy", Ret::Arg0),
        ("lerp", "mix", Ret::Common),
        ("min", "min", Ret::Common),
        ("max", "max", Ret::Common),
        ("pow", "pow", Ret::Common),
        ("step", "step", Ret::Common),
        ("smoothstep", "smoothstep", Ret::Common),
        ("clamp", "clamp", Ret::Common),
        ("atan2", "atan2", Ret::Common),
        ("reflect", "reflect", Ret::Common),
        // Rewritten into `%` after the call is built, because WGSL spells
        // HLSL's `fmod` as an operator and its sign rule is the same.
        ("fmod", "__fmod", Ret::Common),
        ("dot", "dot", Ret::CommonScalar),
        ("distance", "distance", Ret::CommonScalar),
        ("length", "length", Ret::Scalar),
        ("cross", "cross", Ret::Vec3),
        ("determinant", "determinant", Ret::Scalar),
        ("transpose", "transpose", Ret::Arg0),
        ("refract", "refract", Ret::Arg0),
    ];
    TABLE
        .iter()
        .find(|(hlsl, ..)| *hlsl == name)
        .map(|(_, wgsl, ret)| Builtin { wgsl, ret: *ret })
}

/// MilkDrop's own helpers, which `include.fx` defines as macros and the
/// prelude provides as functions.
fn env_helper(name: &str) -> Option<(&'static str, Ty, Ty)> {
    Some(match name {
        "GetPixel" => ("GetPixel", Ty::FLOAT2, Ty::FLOAT3),
        "GetMain" => ("GetMain", Ty::FLOAT2, Ty::FLOAT3),
        "GetBlur1" => ("GetBlur1", Ty::FLOAT2, Ty::FLOAT3),
        "GetBlur2" => ("GetBlur2", Ty::FLOAT2, Ty::FLOAT3),
        "GetBlur3" => ("GetBlur3", Ty::FLOAT2, Ty::FLOAT3),
        _ => return None,
    })
}

/// True when evaluating an expression cannot change anything.
///
/// Deliberately conservative: a call might be a preset's own function, and
/// nothing here proves it has no effect.
fn is_pure(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Number { .. } | ExprKind::Bool(_) | ExprKind::Name(_) => true,
        ExprKind::Member { base, .. } => is_pure(base),
        ExprKind::Index { base, index } => is_pure(base) && is_pure(index),
        ExprKind::Unary { op, expr } => !matches!(op, UnOp::PreInc | UnOp::PreDec) && is_pure(expr),
        ExprKind::Binary { lhs, rhs, .. } => is_pure(lhs) && is_pure(rhs),
        ExprKind::Ternary { cond, then, els } => is_pure(cond) && is_pure(then) && is_pure(els),
        ExprKind::Sequence(items) | ExprKind::InitList(items) => items.iter().all(is_pure),
        _ => false,
    }
}

/// Which of MilkDrop's registers a shader assigns to.
///
/// A pre-pass, because the decision is global: a register written anywhere has
/// to be *read* from the mirror everywhere, or the write is lost the next time
/// the value is used.
fn mirrored_registers(shader: &Shader, pass: Pass) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut note = |name: &str| {
        let target = env::lookup(pass, name)
            .map(|a| a.target.to_owned())
            .or_else(|| env::register(name).map(|_| name.to_owned()));
        if let Some(target) = target {
            let head = target.split('.').next().unwrap_or("");
            if let Some(reg) = head.strip_prefix('_') {
                // The entry point's own inputs are already mutable copies.
                if !matches!(head, "_uv" | "_rad_ang" | "_vDiffuse") {
                    out.insert(reg.to_owned());
                }
            }
        }
    };
    for f in &shader.functions {
        walk_assigned(&f.body, &mut note);
    }
    if let Some(body) = &shader.body {
        walk_assigned(body, &mut note);
    }
    out
}

/// Call `note` with the base name of every assignment target in `block`.
fn walk_assigned(block: &Block, note: &mut impl FnMut(&str)) {
    fn expr(e: &Expr, note: &mut impl FnMut(&str)) {
        match &e.kind {
            ExprKind::Assign { target, value, .. } => {
                if let Some(name) = base_name(target) {
                    note(name);
                }
                expr(value, note);
            }
            ExprKind::PostIncDec { expr: inner, .. } | ExprKind::Unary { expr: inner, .. } => {
                if let Some(name) = base_name(inner) {
                    note(name);
                }
                expr(inner, note);
            }
            ExprKind::Sequence(items) | ExprKind::InitList(items) => {
                for i in items {
                    expr(i, note);
                }
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                expr(lhs, note);
                expr(rhs, note);
            }
            ExprKind::Ternary { cond, then, els } => {
                expr(cond, note);
                expr(then, note);
                expr(els, note);
            }
            ExprKind::Call { args, .. } => {
                for a in args {
                    expr(a, note);
                }
            }
            ExprKind::Member { base, .. } => expr(base, note),
            ExprKind::Index { base, index } => {
                expr(base, note);
                expr(index, note);
            }
            _ => {}
        }
    }
    for s in &block.0 {
        match s {
            Stmt::Expr(e) | Stmt::Return(Some(e)) => expr(e, note),
            Stmt::Decl(vars) => {
                for v in vars {
                    if let Some(i) = &v.init {
                        expr(i, note);
                    }
                }
            }
            Stmt::Block(b) => walk_assigned(b, note),
            Stmt::If { cond, then, els } => {
                expr(cond, note);
                walk_assigned(then, note);
                if let Some(els) = els {
                    walk_assigned(els, note);
                }
            }
            Stmt::While { cond, body } => {
                expr(cond, note);
                walk_assigned(body, note);
            }
            Stmt::DoWhile { body, cond } => {
                walk_assigned(body, note);
                expr(cond, note);
            }
            Stmt::For {
                init,
                cond,
                step,
                body,
            } => {
                if let Some(init) = init {
                    walk_assigned(&Block(vec![(**init).clone()]), note);
                }
                if let Some(c) = cond {
                    expr(c, note);
                }
                if let Some(st) = step {
                    expr(st, note);
                }
                walk_assigned(body, note);
            }
            _ => {}
        }
    }
}

/// The name an assignment target is ultimately a part of.
fn base_name(e: &Expr) -> Option<&str> {
    match &e.kind {
        ExprKind::Name(n) => Some(n),
        ExprKind::Member { base, .. } | ExprKind::Index { base, .. } => base_name(base),
        _ => None,
    }
}

/// A place ending in a multi-lane swizzle, split into what it is a swizzle of
/// and which lanes.
///
/// `None` for a single lane -- `v.x = e` is the one form WGSL does accept.
fn split_swizzle(code: &str) -> Option<(&str, &str)> {
    let (base, last) = code.rsplit_once('.')?;
    if !(2..=4).contains(&last.len()) {
        return None;
    }
    if !last.chars().all(|c| "xyzwrgba".contains(c)) || !is_simple_swizzle(last) {
        return None;
    }
    Some((base, last))
}

/// A scalar built out into a whole matrix, column by column.
///
/// WGSL has no scalar constructor for a matrix and no `+`/`-` between one and
/// a scalar, and HLSL has both. The scalar's text is repeated rather than
/// bound to a name because this is an expression position -- safe here, where
/// nothing a preset can write into an expression has a side effect.
fn splat_matrix(code: &str, ty: Ty) -> String {
    let Ty::Mat { base, rows, cols } = ty else {
        return code.to_owned();
    };
    let col = Ty::Vec { base, n: cols };
    let column = format!("{}({code})", col.wgsl());
    format!("{}({})", ty.wgsl(), vec![column; rows as usize].join(", "))
}

/// The lanes `outer` selects from a value that is already `inner` of a root.
///
/// `compose_swizzle("zw", "x")` is `"z"`: the first lane of `v.zw` is `v.z`.
fn compose_swizzle(inner: &str, outer: &str) -> String {
    let lanes: Vec<char> = inner.chars().collect();
    outer
        .chars()
        .map(|c| {
            let i = lane_index(c).unwrap_or(0);
            *lanes.get(i).unwrap_or(&'x')
        })
        .collect()
}

/// Which component a swizzle letter names, in either of HLSL's two sets.
fn lane_index(c: char) -> Option<usize> {
    "xyzw".find(c).or_else(|| "rgba".find(c))
}

/// True when a swizzle names each lane at most once, which is what makes it
/// assignable: `ret.xy = v` is legal and `ret.xx = v` is not.
fn is_simple_swizzle(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes
        .iter()
        .enumerate()
        .all(|(i, c)| !bytes[..i].contains(c))
}

fn symbol(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::And => "&&",
        BinOp::Or => "||",
    }
}

/// Where an environment name's storage lives in the emitted module.
fn alias_code(target: &str) -> String {
    let (head, tail) = match target.split_once('.') {
        Some((h, t)) => (h, format!(".{t}")),
        None => (target, String::new()),
    };
    match head {
        "_uv" => format!("in_uv{tail}"),
        "_rad_ang" => format!("in_rad_ang{tail}"),
        "_vDiffuse" => format!("in_diffuse{tail}"),
        // `_c2` -> `u.c2`, `_qa` -> `u.qa`.
        h if h.starts_with('_') => format!("u.{}{tail}", &h[1..]),
        h => format!("u.{h}{tail}"),
    }
}

fn array_type(ty: Ty, array: Option<u32>) -> String {
    match array {
        None => ty.wgsl(),
        Some(n) => format!("array<{}, {n}>", ty.wgsl()),
    }
}

fn zero_of(ty: Ty) -> String {
    match ty {
        // `mat2x2<f32>()` is rejected by naga, so a matrix is zeroed one
        // column at a time.
        Ty::Mat { base, rows, cols } => {
            let col = Ty::Vec { base, n: cols };
            let zero = format!("{}()", col.wgsl());
            format!("{}({})", ty.wgsl(), vec![zero; rows as usize].join(", "))
        }
        Ty::Vec {
            base: Base::Bool,
            n: 1,
        } => "false".into(),
        Ty::Vec { n: 1, base } => {
            if base == Base::Int {
                "0".into()
            } else {
                "0.0".into()
            }
        }
        other => format!("{}()", other.wgsl()),
    }
}

fn zero_of_array(ty: Ty, array: Option<u32>) -> String {
    match array {
        None => zero_of(ty),
        Some(n) => format!("array<{}, {n}>()", ty.wgsl()),
    }
}

/// A float literal WGSL will read as an `f32` rather than as an abstract int.
fn format_float(v: f64) -> String {
    if v.is_nan() || v.is_infinite() {
        // A preset cannot write one, but arithmetic on its literals could
        // produce one, and WGSL has no spelling for either.
        return "0.0".into();
    }
    let s = format!("{v:?}");
    if s.contains('.') || s.contains('e') {
        s
    } else {
        format!("{s}.0")
    }
}

/// Whether a block's last statement returns on every path.
///
/// Only as clever as it needs to be: WGSL requires a value-returning function
/// to end in a return, HLSL does not, and a preset whose last branch falls off
/// the end compiled there. Anything this cannot prove gets a trailing zero
/// return, which is what HLSL's own undefined value would most likely have
/// been.
fn ends_with_return(block: &Block) -> bool {
    match block.0.last() {
        Some(Stmt::Return(_)) => true,
        Some(Stmt::Block(b)) => ends_with_return(b),
        Some(Stmt::If {
            then,
            els: Some(els),
            ..
        }) => ends_with_return(then) && ends_with_return(els),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::milk::hlsl::parse::parse;

    fn wgsl(body: &str) -> String {
        let shader = parse(&format!("shader_body {{ {body} }}")).expect("parses");
        emit(&shader, Pass::Warp).expect("emits").source
    }

    fn whole(src: &str) -> String {
        let shader = parse(src).expect("parses");
        emit(&shader, Pass::Warp).expect("emits").source
    }

    fn fails(body: &str) -> HlslError {
        let shader = parse(&format!("shader_body {{ {body} }}")).expect("parses");
        emit(&shader, Pass::Warp).unwrap_err()
    }

    #[test]
    fn the_three_reshapings_hlsl_does_silently_are_all_written_out() {
        // `float d = texsize.zw;` -- a float2 truncated to a scalar.
        assert!(wgsl("float d = texsize.zw;").contains("var p_d: f32 = ((u.c7).zw).x;"));
        // `float2 v = 0.008;` -- a scalar splatted.
        assert!(wgsl("float2 v = 0.008;").contains("var p_v: vec2<f32> = vec2<f32>(0.008);"));
        // A bool multiplied into a float3.
        let out = wgsl("ret = (rad < 1)*ret;");
        assert!(
            out.contains("vec3<f32>("),
            "the comparison becomes a float3"
        );
    }

    #[test]
    fn an_integer_literal_stays_an_integer_and_a_float_gets_a_point() {
        let out = wgsl("float a = 1; float b = 1.5;");
        // `1` is an i32 in HLSL, so the conversion to f32 is written rather
        // than assumed -- which is what keeps `1/2` equal to zero.
        assert!(out.contains("f32(1)"), "{out}");
        assert!(out.contains("= 1.5;"), "{out}");
    }

    #[test]
    fn every_preset_name_is_prefixed_and_the_environment_is_not() {
        let out = wgsl("float lum2 = 1; ret = uv.x + time + q17 + lum2;");
        assert!(out.contains("p_lum2"));
        assert!(out.contains("in_uv"), "uv is a parameter");
        assert!(out.contains("u.c2.x"), "time is a register lane");
        assert!(out.contains("u.qe.x"), "q17 is a register lane");
    }

    #[test]
    fn a_preset_may_declare_a_function_named_like_a_builtin() {
        // HLSL's `lum` is a macro, so a preset's own `lum` would not have won
        // there either -- and prefixing means neither shadows the other here.
        let out = whole("float lum(float x){ return x; } shader_body { ret = lum(uv).xxx; }");
        assert!(out.contains("fn p_lum("));
        assert!(out.contains("fn lum("), "the prelude's is still there");
    }

    #[test]
    fn a_texture_read_becomes_an_explicit_lod_sample() {
        // Never `textureSample`: a quarter of the corpus branches, and WGSL
        // forbids implicit derivatives under non-uniform control flow.
        let out = wgsl("ret = tex2D(sampler_main, uv).xyz;");
        assert!(out.contains("milk_tex2D(tex_main, samp_fw,"), "{out}");
        assert!(!out.contains("textureSample(") || out.contains("textureSampleLevel"));
    }

    #[test]
    fn the_sampler_name_chooses_the_filter_and_address_mode() {
        assert!(wgsl("ret = tex2D(sampler_pc_main, uv).xyz;").contains("tex_main, samp_pc"));
        assert!(wgsl("ret = tex2D(sampler_fc_main, uv).xyz;").contains("tex_main, samp_fc"));
        assert!(wgsl("ret = tex3D(sampler_noisevol_hq, uv.xyy).xyz;")
            .contains("milk_tex3D(tex_noisevol_hq"));
    }

    #[test]
    fn a_texture_this_build_does_not_load_draws_a_placeholder_and_is_named() {
        let shader = parse(
            "sampler sampler_pc_clouds; shader_body { ret = tex2D(sampler_pc_clouds, uv).xyz; }",
        )
        .expect("parses");
        let out = emit(&shader, Pass::Warp).expect("emits");
        assert!(out.source.contains("tex_placeholder"));
        assert_eq!(out.wants_textures, vec!["clouds".to_owned()]);
    }

    #[test]
    fn hlsl_truthiness_becomes_an_explicit_comparison() {
        let out = wgsl("if (rad) { ret = 0; }");
        assert!(out.contains("!= 0.0)"), "{out}");
    }

    #[test]
    fn a_ternary_becomes_select_with_its_operands_the_right_way_round() {
        // `select(false, true, cond)` -- written the intuitive way it is a
        // silent inversion.
        let out = wgsl("float a = rad > 0 ? 1.0 : 2.0;");
        assert!(out.contains("select(2.0, 1.0, "), "{out}");
    }

    #[test]
    fn every_loop_gets_a_runaway_guard() {
        // A hung fragment shader is a hung display, not a slow frame.
        let out = wgsl("for (int i=0; i<4; i++) { ret += 1; }");
        assert!(out.contains("milk_guard1"), "{out}");
        assert!(out.contains(&format!("> {MAX_LOOP}")), "{out}");
        assert!(out.contains("continuing {"), "the step runs on continue");
    }

    #[test]
    fn a_for_with_two_updates_keeps_both() {
        let out = wgsl("int j = 0; for (int i=0; i<4; i++, j++) { ret += 1; }");
        assert!(out.contains("p_i = p_i + 1"), "{out}");
        assert!(out.contains("p_j = p_j + 1"), "{out}");
    }

    #[test]
    fn a_compound_assignment_truncates_the_way_hlsl_does() {
        // `ret.x += texsize.zw` keeps only the first lane, which WGSL's own
        // `+=` would refuse outright.
        let out = wgsl("ret.x += texsize.zw;");
        // And the target is not parenthesised: WGSL rejects `(p_ret).x = ...`
        // outright, which is a whole-corpus failure from one defensive habit.
        assert!(out.contains("p_ret.x = "), "{out}");
        assert!(!out.contains("(p_ret).x ="), "{out}");
    }

    #[test]
    fn an_initializer_list_is_regrouped_by_the_targets_shape() {
        // Twenty scalars for ten pairs is how the corpus writes it.
        let out = wgsl("float2 o[2] = { 1.0, 2.0, 3.0, 4.0 };");
        assert!(out.contains("array<vec2<f32>, 2>("), "{out}");
        assert!(out.contains("vec2<f32>(1.0, 2.0)"), "{out}");
        assert!(out.contains("vec2<f32>(3.0, 4.0)"), "{out}");
    }

    #[test]
    fn a_global_is_private_storage_initialised_at_entry() {
        // WGSL wants module-scope initializers to be const-expressions, and
        // the corpus initializes globals with `sin(...)`.
        let out = whole("static const float k = sin(1.0); shader_body { ret = k; }");
        assert!(out.contains("var<private> p_k: f32;"), "{out}");
        assert!(out.contains("fn milk_init_globals()"), "{out}");
        assert!(out.contains("milk_init_globals();"), "{out}");
    }

    #[test]
    fn a_value_returning_function_always_ends_in_a_return() {
        // WGSL requires it and HLSL does not.
        let out = whole(
            "float f(float x){ if (x > 0) { return 1.0; } } shader_body { ret = f(rad).xxx; }",
        );
        assert!(out.contains("return 0.0;"), "{out}");
    }

    #[test]
    fn mul_multiplies_the_other_way_round() {
        // HLSL's `mul(v, m)` is a row vector times a matrix. With the matrix
        // stored column-for-row (see `ty::Ty::wgsl`), the same arithmetic is
        // WGSL's `m * v` -- so the operands swap, and this is the one place
        // that mapping costs anything.
        let out = wgsl("float3 v = mul(float4(1,2,3,4), rot_s1);");
        assert!(out.contains("(u.rot_s1 * "), "{out}");
    }

    #[test]
    fn a_matrix_is_built_from_its_columns() {
        // `mat2x2<f32>(a, b, c, d)` is not accepted by naga, and HLSL's
        // row-major order groups into exactly the columns this mapping wants.
        let out = wgsl("float2x2 m = float2x2(1,2,3,4); float2 r = mul(uv, m);");
        assert!(
            out.contains("mat2x2<f32>(vec2<f32>(f32(1), f32(2)), vec2<f32>(f32(3), f32(4)))"),
            "{out}"
        );
    }

    #[test]
    fn milkdrops_constants_and_its_raw_registers_both_resolve() {
        let out = wgsl("ret = _qa.xyz * M_PI + _c7.x * M_INV_PI_2;");
        assert!(out.contains("u.qa"), "{out}");
        assert!(out.contains("u.c7"), "{out}");
        assert!(out.contains("3.14159265359"), "{out}");
    }

    #[test]
    fn lum_truncates_the_way_the_dot_it_is_made_of_does() {
        // `lum(roam_sin.xy)` is legal HLSL -- the macro expands to a `dot`,
        // and `dot` truncates to the narrower operand.
        let out = wgsl("float a = lum(roam_sin.xy);");
        assert!(out.contains("vec2<f32>(0.32, 0.49)"), "{out}");
        let out = wgsl("float a = lum(GetPixel(uv));");
        assert!(out.contains("lum(") || out.contains("0.29"), "{out}");
    }

    #[test]
    fn an_expression_with_no_effect_is_kept_rather_than_refused() {
        // `texsize.zw*r;` on a line of its own is in the corpus; HLSL warns
        // and compiles it.
        let out = wgsl("float r = 1.0; texsize.zw*r;");
        assert!(out.contains("_ = "), "{out}");
    }

    #[test]
    fn a_swizzle_of_a_swizzle_composes_so_it_stays_assignable() {
        // `uv` is `in_uv.xy`, so `uv.x` must be `in_uv.x` -- chained as
        // `in_uv.xy.x` it reads correctly and cannot be assigned to, and
        // `uv.x = ...` is an ordinary line in these presets.
        let out = wgsl("uv.x = 0.5;");
        assert!(out.contains("in_uv.x = "), "{out}");
        assert!(!out.contains("in_uv.xy.x"), "{out}");
        // And the composition is by position, not by name.
        let out = wgsl("float a = uv_orig.y;");
        assert!(
            out.contains("in_uv.w"),
            "uv_orig is _uv.zw, so .y is .w: {out}"
        );
    }

    #[test]
    fn multiplying_two_comparisons_promotes_them_to_numbers() {
        // A branch-free `and`, which the corpus writes and WGSL has no
        // arithmetic for.
        let out = wgsl("float a = (rad > 1) * (rad < 2);");
        assert!(out.contains("f32("), "{out}");
        assert!(!out.contains("bool) *"), "{out}");
    }

    #[test]
    fn what_the_emitter_cannot_translate_says_so_by_name() {
        assert_eq!(
            fails("ret = nosuchfunction(uv);").construct,
            "unknown-function"
        );
        assert_eq!(fails("ret = nosuchname;").construct, "unknown-name");
        assert_eq!(fails("ret = uv.zzz;").construct, "swizzle");
        assert_eq!(fails("ret = sampler_main;").construct, "sampler-as-value");
        assert_eq!(
            fails("float4 a = uv;").construct,
            "conversion",
            "a float2 never widens to a float4"
        );
    }

    #[test]
    fn the_wrapper_matches_milkdrops_own_first_and_last_lines() {
        let out = wgsl("ret = GetPixel(uv);");
        assert!(out.contains("var p_ret: vec3<f32> = vec3<f32>(0.0);"));
        assert!(out.contains("return vec4<f32>(p_ret.xyz, in_diffuse.w);"));
    }
}
