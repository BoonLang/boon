## Part 9: Virtual Filesystem, Modules, and Multi-Renderer Support

This section covers the infrastructure needed to support complex examples like `todo_mvc_physical`:
- Multi-file projects with BUILD.bn/RUN.bn pattern
- Directory-based modules (Theme/Professional.bn)
- Generated files (Generated/Assets.bn)
- Multiple renderers (Zoon for 2D web, RayBox for 3D GPU)

### 9.1 Virtual Filesystem (Arena-Native)

The VirtualFilesystem provides thread-safe file operations for the arena-based engine.

#### VirtualFilesystem Structure

```rust
/// Thread-safe virtual filesystem
pub struct VirtualFilesystem {
    /// Files stored in arena as Text payloads
    files: HashMap<String, String>,
}

impl VirtualFilesystem {
    pub fn new() -> Self {
        Self { files: HashMap::new() }
    }

    pub fn with_files(files: Vec<(String, String)>) -> Self {
        Self { files: files.into_iter().collect() }
    }

    /// Normalize path: remove leading/trailing slashes, "./" prefixes
    pub fn normalize_path(path: &str) -> String {
        path.trim_start_matches("./")
            .trim_start_matches('/')
            .trim_end_matches('/')
            .to_string()
    }

    pub fn read_text(&self, path: &str) -> Option<&str> {
        let normalized = Self::normalize_path(path);
        self.files.get(&normalized).map(|s| s.as_str())
    }

    pub fn write_text(&mut self, path: &str, content: String) {
        let normalized = Self::normalize_path(path);
        self.files.insert(normalized, content);
    }

    pub fn list_directory(&self, path: &str) -> Vec<String> {
        let normalized = Self::normalize_path(path);
        let prefix = if normalized.is_empty() { String::new() } else { format!("{}/", normalized) };

        self.files.keys()
            .filter(|k| k.starts_with(&prefix))
            .map(|k| k[prefix.len()..].split('/').next().unwrap().to_string())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn exists(&self, path: &str) -> bool {
        let normalized = Self::normalize_path(path);
        self.files.contains_key(&normalized)
    }

    pub fn delete(&mut self, path: &str) -> bool {
        let normalized = Self::normalize_path(path);
        self.files.remove(&normalized).is_some()
    }
}
```

#### File API Functions

```rust
pub struct FileReadText;

impl BoonFunction for FileReadText {
    fn path(&self) -> &'static [&'static str] { &["File", "read_text"] }
    fn min_args(&self) -> usize { 1 }

    fn call(&self, args: Arguments, ctx: FunctionContext) -> SlotId {
        let path_slot = args.expect_exact::<1>()[0];

        // Create Producer node that reads file
        let output_slot = ctx.arena.alloc_producer(
            ctx.source_id,
            ctx.scope_id,
            || {
                let path = ctx.arena.get_value(path_slot).expect_text("File/read_text path");
                match ctx.virtual_fs.read_text(&path) {
                    Some(content) => Payload::Text(content.into()),
                    None => Payload::Tag(ctx.arena.intern_tag("None")),
                }
            },
        );

        output_slot
    }
}

pub struct DirectoryEntries;

impl BoonFunction for DirectoryEntries {
    fn path(&self) -> &'static [&'static str] { &["Directory", "entries"] }
    fn min_args(&self) -> usize { 1 }

    fn call(&self, args: Arguments, ctx: FunctionContext) -> SlotId {
        let path_slot = args.expect_exact::<1>()[0];

        // Create Bus node with directory entries
        let entries = ctx.virtual_fs.list_directory(
            &ctx.arena.get_value(path_slot).expect_text("Directory/entries path")
        );

        let bus_slot = ctx.arena.alloc_bus(ctx.source_id, ctx.scope_id);

        for entry in entries {
            let entry_slot = ctx.arena.alloc_producer(
                ctx.source_id.child(1),
                ctx.scope_id,
                Payload::Text(entry.into()),
            );
            ctx.arena.bus_append(bus_slot, entry_slot);
        }

        bus_slot
    }
}
```

---

### 9.2 Module System

The ModuleLoader handles parsing, caching, and resolving module references.

#### Module Data

```rust
/// Parsed module data
pub struct ModuleData {
    pub source_path: String,
    pub functions: HashMap<String, FunctionDef>,
    pub variables: HashMap<String, SlotId>,
}

/// Cached function definition
pub struct FunctionDef {
    pub params: Vec<String>,
    pub body_ast: Arc<Expression>,
}
```

#### ModuleLoader

```rust
pub struct ModuleLoader {
    /// Loaded modules by name
    cache: HashMap<String, ModuleData>,
    /// Base directory for resolution
    base_dir: String,
}

impl ModuleLoader {
    /// Resolution order for module "Theme":
    /// 1. {base_dir}/Theme.bn
    /// 2. {base_dir}/Theme/Theme.bn  (directory-based)
    /// 3. {base_dir}/Generated/Theme.bn (generated files)
    pub fn resolve_module_path(&self, name: &str, virtual_fs: &VirtualFilesystem) -> Option<String> {
        let paths = vec![
            format!("{}/{}.bn", self.base_dir, name),
            format!("{}/{}/{}.bn", self.base_dir, name, name),
            format!("{}/Generated/{}.bn", self.base_dir, name),
        ];

        for path in paths {
            if virtual_fs.exists(&path) {
                return Some(path);
            }
        }
        None
    }

    pub fn load_module(
        &mut self,
        name: &str,
        virtual_fs: &VirtualFilesystem,
        parser: &Parser,
    ) -> Option<&ModuleData> {
        // Return cached if available
        if self.cache.contains_key(name) {
            return self.cache.get(name);
        }

        // Resolve path
        let path = self.resolve_module_path(name, virtual_fs)?;

        // Read and parse
        let source = virtual_fs.read_text(&path)?;
        let ast = parser.parse(source).ok()?;

        // Extract functions and variables
        let module_data = self.extract_module_data(&path, &ast);

        self.cache.insert(name.to_string(), module_data);
        self.cache.get(name)
    }
}
```

#### Module Function Resolution

```rust
impl Arena {
    /// Resolve ModuleName/function call
    pub fn resolve_module_function(
        &mut self,
        module_name: &str,
        function_name: &str,
        args: &[SlotId],
        ctx: &FunctionContext,
    ) -> SlotId {
        let module = ctx.module_loader.load_module(module_name, &ctx.virtual_fs, &ctx.parser)
            .unwrap_or_else(|| panic!("Module '{}' not found", module_name));

        let func_def = module.functions.get(function_name)
            .unwrap_or_else(|| panic!("Function '{}/{}' not found", module_name, function_name));

        // Create scope for function body evaluation
        let func_scope = ctx.scope_id.child(hash(module_name, function_name));

        // Bind parameters to argument slots
        for (i, param) in func_def.params.iter().enumerate() {
            let arg_slot = args.get(i).copied()
                .unwrap_or_else(|| panic!("{}/{}(..) missing argument '{}'", module_name, function_name, param));
            ctx.arena.bind_variable(func_scope, param, arg_slot);
        }

        // Evaluate function body
        ctx.evaluator.evaluate_expression(&func_def.body_ast, func_scope)
    }
}
```

---

### 9.3 BUILD.bn / RUN.bn Pattern

Support for build-time code generation before main execution.

#### Execution Flow

```
1. Load all project files into VirtualFilesystem
2. If BUILD.bn exists:
   a. Parse and evaluate BUILD.bn
   b. Allow File/write_text to modify VirtualFilesystem
   c. Wait for completion
3. Parse and evaluate RUN.bn (main entry point)
4. Generated/ files available via module imports
```

#### Interpreter Integration

```rust
pub struct Interpreter {
    arena: Arena,
    event_loop: EventLoop,
    virtual_fs: VirtualFilesystem,
    module_loader: ModuleLoader,
}

impl Interpreter {
    pub fn run_project(
        &mut self,
        files: Vec<(String, String)>,
        main_file: &str,
    ) -> Result<SlotId, Error> {
        // Initialize virtual filesystem
        self.virtual_fs = VirtualFilesystem::with_files(files);

        // Run BUILD.bn if exists
        if self.virtual_fs.exists("BUILD.bn") {
            let build_source = self.virtual_fs.read_text("BUILD.bn").unwrap();
            self.evaluate_file("BUILD.bn", build_source)?;
            // Process ticks until quiescent (file generation complete)
            self.event_loop.run_until_quiescent(&mut self.arena);
        }

        // Run main file
        let main_source = self.virtual_fs.read_text(main_file)
            .ok_or_else(|| Error::FileNotFound(main_file.to_string()))?;

        self.evaluate_file(main_file, main_source)
    }
}
```

---

### 9.4 Multi-Renderer Architecture

Support for multiple rendering backends (Zoon for web, RayBox for 3D GPU).

#### Renderer Trait

```rust
/// Trait for rendering backends
pub trait Renderer: Send + Sync {
    /// Element kinds this renderer supports
    fn supported_elements(&self) -> &[ElementKind];

    /// Render an element to this backend
    fn render_element(&mut self, element: &ElementNode, arena: &Arena);

    /// Inject DOM event into the arena
    fn inject_event(&self, element_id: SlotId, event: EventKind, payload: Payload);

    /// Called each tick to sync state
    fn sync(&mut self, arena: &Arena);
}

/// Zoon renderer (current 2D web)
pub struct ZoonRenderer {
    rendered_elements: HashMap<SlotId, ZoonElementHandle>,
}

impl Renderer for ZoonRenderer {
    fn supported_elements(&self) -> &[ElementKind] {
        &[
            ElementKind::Button, ElementKind::Stripe, ElementKind::Container,
            ElementKind::Stack, ElementKind::TextInput, ElementKind::Checkbox,
            ElementKind::Label, ElementKind::Paragraph, ElementKind::Link,
        ]
    }
    // ...
}

/// RayBox renderer (future 3D GPU)
pub struct RayBoxRenderer {
    scene: Scene,
    lights: Vec<Light>,
}

impl Renderer for RayBoxRenderer {
    fn supported_elements(&self) -> &[ElementKind] {
        &[
            ElementKind::Button, ElementKind::Stripe, ElementKind::Block,
            ElementKind::TextInput, ElementKind::Checkbox, ElementKind::Text,
        ]
    }
    // ...
}
```

#### Document/new with Renderer Selection

```rust
pub struct DocumentNew;

impl BoonFunction for DocumentNew {
    fn path(&self) -> &'static [&'static str] { &["Document", "new"] }

    fn call(&self, args: Arguments, ctx: FunctionContext) -> SlotId {
        let root_slot = args.require(0, "root");

        // Create Document node
        let doc_slot = ctx.arena.alloc_document(ctx.source_id, ctx.scope_id, root_slot);

        // Register with active renderer
        ctx.renderer.register_document(doc_slot);

        doc_slot
    }
}
```

---

### 9.5 Scene API for 3D Rendering

API functions for RayBox 3D rendering (Scene/*, Light/*).

#### Scene/new

```rust
pub struct SceneNew;

impl BoonFunction for SceneNew {
    fn path(&self) -> &'static [&'static str] { &["Scene", "new"] }
    fn min_args(&self) -> usize { 1 }

    fn call(&self, args: Arguments, ctx: FunctionContext) -> SlotId {
        let root_slot = args.require(0, "root");
        let lights_slot = args.get(1);  // Optional lights list
        let geometry_slot = args.get(2); // Optional geometry settings

        // Create Scene node
        let scene_slot = ctx.arena.alloc_scene(
            ctx.source_id,
            ctx.scope_id,
            root_slot,
            lights_slot,
            geometry_slot,
        );

        scene_slot
    }
}
```

#### Light Functions

```rust
pub struct LightDirectional;

impl BoonFunction for LightDirectional {
    fn path(&self) -> &'static [&'static str] { &["Light", "directional"] }

    fn call(&self, args: Arguments, ctx: FunctionContext) -> SlotId {
        // Create Router with light properties
        let azimuth = args.get(0).unwrap_or_else(|| ctx.arena.alloc_producer_number(0.0));
        let altitude = args.get(1).unwrap_or_else(|| ctx.arena.alloc_producer_number(45.0));
        let intensity = args.get(2).unwrap_or_else(|| ctx.arena.alloc_producer_number(1.0));

        ctx.arena.alloc_tagged_object(
            ctx.source_id,
            ctx.scope_id,
            "DirectionalLight",
            vec![
                ("azimuth", azimuth),
                ("altitude", altitude),
                ("intensity", intensity),
            ],
        )
    }
}

pub struct LightAmbient;

impl BoonFunction for LightAmbient {
    fn path(&self) -> &'static [&'static str] { &["Light", "ambient"] }

    fn call(&self, args: Arguments, ctx: FunctionContext) -> SlotId {
        let intensity = args.get(0).unwrap_or_else(|| ctx.arena.alloc_producer_number(0.4));

        ctx.arena.alloc_tagged_object(
            ctx.source_id,
            ctx.scope_id,
            "AmbientLight",
            vec![("intensity", intensity)],
        )
    }
}
```

---

### 9.6 3D Element Properties

Extended Element properties for physically-based rendering.

#### 3D Style Properties

```rust
/// Extended style properties for 3D rendering
pub struct Style3DProperties {
    // Existing 2D properties
    pub width: Option<SlotId>,
    pub height: Option<SlotId>,
    pub padding: Option<SlotId>,
    pub background_color: Option<SlotId>,
    pub font: Option<SlotId>,

    // 3D-specific properties
    pub depth: Option<SlotId>,           // Z-thickness
    pub material: Option<SlotId>,        // [gloss, metal, shine]
    pub relief: Option<SlotId>,          // Raised | Carved | Normal
    pub move_position: Option<SlotId>,   // [closer: N] or [further: N]
    pub rounded_corners: Option<SlotId>, // 3D edge radius
}
```

#### Material Object

```boon
-- In Boon code (todo_mvc_physical style)
Element/button(
    style: [
        depth: 6
        material: [gloss: 0.7, metal: 0.0, shine: 0.3]
        relief: Raised
        move: [closer: 2]  -- Sits 2 units above parent
    ]
    label: TEXT { Click me }
)
```

#### Bridge Handling

```rust
impl RayBoxRenderer {
    fn extract_3d_properties(&self, settings_slot: SlotId, arena: &Arena) -> Style3DProperties {
        Style3DProperties {
            depth: arena.try_navigate_field(settings_slot, &["style", "depth"]),
            material: arena.try_navigate_field(settings_slot, &["style", "material"]),
            relief: arena.try_navigate_field(settings_slot, &["style", "relief"]),
            move_position: arena.try_navigate_field(settings_slot, &["style", "move"]),
            // ... etc
        }
    }
}
```

---

### 9.7 todo_mvc_physical Compatibility

Ensure the new engine supports all features needed by `todo_mvc_physical`:

#### Required Features Checklist

| Feature | Status | Implementation |
|---------|--------|----------------|
| Multi-file projects | ✅ Planned | VirtualFilesystem + ModuleLoader |
| BUILD.bn execution | ✅ Planned | Interpreter.run_project() |
| Directory modules (Theme/Theme.bn) | ✅ Planned | ModuleLoader resolution order |
| Generated/ directory | ✅ Planned | ModuleLoader.resolve_module_path() |
| File/read_text | ✅ Planned | BoonFunction implementation |
| File/write_text | ✅ Planned | BoonFunction implementation |
| Directory/entries | ✅ Planned | BoonFunction implementation |
| Scene/new | ✅ Planned | For RayBox 3D rendering |
| Light/* | ✅ Planned | DirectionalLight, AmbientLight |
| Theme module pattern | ✅ Planned | WHEN dispatch + module calls |
| 3D style properties | ✅ Planned | Style3DProperties |
| Material properties | ✅ Planned | Payload::ObjectHandle fields |

#### Module Structure from todo_mvc_physical

```
todo_mvc_physical/
├── BUILD.bn          → File/*, Directory/* functions
├── RUN.bn            → Main entry, Scene/new
├── Theme/
│   ├── Theme.bn      → Module dispatcher (WHEN pattern)
│   ├── Professional.bn
│   ├── Glassmorphism.bn
│   ├── Neobrutalism.bn
│   └── Neumorphism.bn
├── Generated/
│   └── Assets.bn     → Generated by BUILD.bn
└── assets/
    └── icons/        → Source SVG files
```

---

### 9.8 Implementation Phases

#### Part of Phase 7: Bridge & UI (Extended)

Add to existing Phase 7:
1. Implement VirtualFilesystem (arena-native)
2. Implement ModuleLoader with caching
3. Implement File/*, Directory/* functions
4. Implement BUILD.bn/RUN.bn pattern in Interpreter

#### Phase 9: Multi-Renderer Support (Future)

1. Define Renderer trait
2. Extract ZoonRenderer from current Bridge
3. Implement Scene/new, Light/* functions
4. Implement Style3DProperties extraction
5. Stub RayBoxRenderer interface
6. **Milestone:** `todo_mvc_physical` runs with Zoon (2D fallback)

#### Phase 10: RayBox Integration (Future)

1. Integrate RayBox renderer from ~/repos/RayBox
2. Implement RayBoxRenderer
3. Add 3D geometry generation
4. Add WebGPU pipeline integration
5. **Milestone:** `todo_mvc_physical` renders in 3D

---

### 9.9 Critical Files

| File | Purpose |
|------|---------|
| `engine_v2/virtual_fs.rs` | **NEW:** VirtualFilesystem |
| `engine_v2/module_loader.rs` | **NEW:** ModuleLoader with caching |
| `engine_v2/functions/file.rs` | **NEW:** File/*, Directory/* |
| `engine_v2/functions/scene.rs` | **NEW:** Scene/new, Light/* |
| `engine_v2/renderer.rs` | **NEW:** Renderer trait |
| `engine_v2/renderers/zoon.rs` | **NEW:** ZoonRenderer |
| `engine_v2/renderers/raybox.rs` | **FUTURE:** RayBoxRenderer |

---

