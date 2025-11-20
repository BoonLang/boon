# GPU Shader Research: What Boon Cannot Express

**Date:** 2025-11-20
**Type:** Capability Gap Analysis
**Focus:** GPU Shaders (WGSL, Compute Shaders) vs Boon Language
**Approach:** Bottom-up - What can GPUs do that Boon cannot express?

---

## Executive Summary

**Question:** Could Boon be used to write GPU shaders (WGSL/WebGPU)?

**Initial Analysis (Top-down):** ~60% of Boon's functional core would work for shaders, but reactive features (LATEST, LINK, PASSED.clk) don't map to GPU's spatial parallelism.

**This Analysis (Bottom-up):** GPU shaders have fundamental capabilities that **do not exist** in Boon's execution model:
- Explicit parallelism primitives (work groups, thread IDs)
- Memory hierarchy (address spaces)
- Synchronization (barriers, atomics)
- Spatial communication (derivatives, subgroups)
- Hardware integration (textures, rasterization)

**Conclusion:** Boon could express the **algorithms** but not the **parallel execution model**, **memory management**, or **hardware features** that make GPU shaders effective.

**Fundamental Insight:**
- Boon → HDL: **Accidental success** (reactive patterns naturally map to clocked circuits)
- Boon → GPU: **Fundamental mismatch** (temporal reactivity vs spatial parallelism)

---

## Critical Missing Capabilities

### The 5 Critical Gaps

| Gap | Severity | Boon Status | GPU Requirement |
|-----|----------|-------------|-----------------|
| **Parallelism Model** | 🔴 CRITICAL | ❌ None | Work groups, thread IDs, dispatch |
| **Memory Address Spaces** | 🔴 CRITICAL | ❌ None | Private, workgroup, uniform, storage |
| **Barriers/Synchronization** | 🔴 CRITICAL | ❌ None | `workgroupBarrier()`, memory fences |
| **Atomic Operations** | 🔴 CRITICAL | ❌ None | `atomicAdd()`, `atomicMax()`, CAS |
| **Derivative Functions** | 🟡 MAJOR | ❌ None | `dpdx()`, `dpdy()` - neighbor access |

**Without these 5, you cannot write functional GPU shaders.**

---

## Detailed Gap Analysis

### 1. Explicit Parallelism Model ⚠️ CRITICAL GAP

#### What GPUs Have

```wgsl
@compute @workgroup_size(16, 16, 1)
fn process_image(
    @builtin(global_invocation_id) global_id: vec3<u32>,
    @builtin(local_invocation_id) local_id: vec3<u32>,
    @builtin(workgroup_id) workgroup_id: vec3<u32>,
) {
    // This function runs 256 times IN PARALLEL (16×16)
    // Each invocation knows its position in the 3D grid
    let pixel_index = global_id.xy;
    let thread_in_group = local_id.xy;

    // Process one pixel per thread
    output[pixel_index.y * width + pixel_index.x] = compute(pixel_index);
}

// Dispatch: Run this on 1920×1080 image
// GPU launches (1920/16) × (1080/16) = 120 × 68 = 8,160 work groups
// Each work group has 16×16 = 256 threads
// Total: 8,160 × 256 = 2,088,960 threads running in parallel!
```

#### What Boon Has

```boon
-- Sequential iteration or elaboration-time unrolling
pixels: List/range(0, width * height)
    |> List/map(index: process_pixel(index))
```

**This is NOT parallel execution:**
- `List/map` with dynamic list → sequential iteration
- `List/map` with fixed-size list → elaboration-time unrolling (generates N instances at compile time)
- Neither expresses "run this function 2 million times simultaneously at runtime"

#### What Boon Lacks

- No concept of **work group size** (how many threads per group)
- No **thread IDs** (which invocation am I?)
- No **dispatch dimensions** (launch N×M×K work groups)
- No way to express "run this function massively parallel at runtime"

#### What Would Be Needed

```boon
-- Hypothetical Boon GPU syntax
COMPUTE[workgroup: 16, 16, 1]
FUNCTION process_image(
    BUILTIN global_id: Vec3U32,
    BUILTIN local_id: Vec3U32,
    BUILTIN workgroup_id: Vec3U32
) {
    pixel_index: global_id.xy
    result[pixel_index.y * width + pixel_index.x] = compute(pixel_index)
}

-- Dispatch would be external: run on 1920×1080 grid
```

#### Why This Is Critical

**Without explicit parallelism primitives, you cannot:**
- Express how work is divided across threads
- Know which thread you are (for indexing into arrays)
- Control work group sizes (affects shared memory, barriers)
- Launch millions of threads simultaneously

---

### 2. Memory Address Spaces ⚠️ CRITICAL GAP

#### What GPUs Have

```wgsl
// Different memory spaces with vastly different performance
var<private> temp: f32;                    // Thread-local (registers) - FASTEST
var<workgroup> shared_data: array<f32, 256>; // Shared in work group - FAST
@group(0) @binding(0) var<uniform> params: Params;      // Read-only, cached - MEDIUM
@group(0) @binding(1) var<storage, read_write> data: array<f32>; // Global - SLOW

fn example() {
    // Private: Each thread has its own copy (registers)
    temp = 42.0;  // 0 latency

    // Workgroup: Shared across 256 threads in this work group
    shared_data[local_id] = temp;  // ~10 cycles
    workgroupBarrier();
    let neighbor = shared_data[local_id + 1];  // Can read other threads' data!

    // Uniform: All threads read same values (broadcast)
    let camera_pos = params.camera.position;  // ~100 cycles, cached

    // Storage: Global memory, slowest
    data[global_id] = result;  // ~400 cycles
}
```

#### Performance Difference

```
Private (registers):     1x   (baseline)
Workgroup (shared):     10x   slower
Uniform (constant):    100x   slower (but cached, amortized)
Storage (global):      400x   slower
Texture (specialized): 100x   slower (but massive bandwidth)
```

#### What Boon Has

```boon
-- Just variables
temp: 42.0
data: [1, 2, 3, 4]
result: compute(data)

-- No concept of:
-- - Where this is stored (registers? memory?)
-- - Who can access it (just this thread? all threads?)
-- - How fast access is
-- - Whether it's read-only or read-write
```

**Boon has a flat memory model** - all variables are just "somewhere".

#### What Boon Lacks

- No **address space qualifiers** (`private`, `workgroup`, `uniform`, `storage`)
- No **binding annotations** (`@group(0) @binding(1)`)
- No distinction between **thread-local** vs **shared** vs **global** memory
- No way to express **performance-critical memory placement**

#### Why This Matters

**Example: Parallel Reduction**

```wgsl
// FAST version using workgroup memory
var<workgroup> partial_sums: array<f32, 256>;

@compute @workgroup_size(256)
fn sum_reduction(@builtin(local_invocation_id) tid: vec3<u32>) {
    // Phase 1: Each thread computes local sum
    var sum = 0.0;
    for (var i = tid.x; i < count; i += 256) {
        sum += data[i];
    }
    partial_sums[tid.x] = sum;  // Write to shared memory
    workgroupBarrier();  // Wait for all threads

    // Phase 2: Parallel tree reduction (shared memory)
    for (var stride = 128; stride > 0; stride /= 2) {
        if (tid.x < stride) {
            partial_sums[tid.x] += partial_sums[tid.x + stride];
        }
        workgroupBarrier();
    }

    // Thread 0 writes final result
    if (tid.x == 0) {
        output[workgroup_id.x] = partial_sums[0];
    }
}
// Performance: ~10 microseconds for 1 million elements
```

**Without workgroup memory:** Would need global atomics, 100x slower!

#### What Would Be Needed

```boon
-- Explicit memory space declarations
SHARED partial_sums: ARRAY { 256, Float32 }  -- Workgroup shared
UNIFORM camera: Camera                        -- Read-only uniform
STORAGE particles: ARRAY { 10000, Particle }  -- Global read-write
PRIVATE temp: Float32                         -- Thread-local

-- Binding annotations
UNIFORM[group: 0, binding: 0] transform: Mat4
STORAGE[group: 0, binding: 1] output: ARRAY { 1000, Vec4 }
```

---

### 3. Synchronization Primitives ⚠️ CRITICAL GAP

#### What GPUs Have

```wgsl
// Barrier: All threads in work group wait here
workgroupBarrier();

// Memory barriers: Ensure visibility of writes
storageBarrier();  // Storage buffer writes visible to all threads

// Example: Safe shared memory communication
var<workgroup> scratch: array<f32, 256>;

@compute @workgroup_size(256)
fn example(@builtin(local_invocation_id) tid: vec3<u32>) {
    // Phase 1: Each thread writes its data
    scratch[tid.x] = compute_value(tid.x);

    workgroupBarrier();  // CRITICAL: Wait for ALL threads to finish writing

    // Phase 2: Each thread reads from neighbors
    let left = scratch[tid.x - 1];   // Safe: all writes completed
    let right = scratch[tid.x + 1];  // Safe: all writes completed
    let result = (left + scratch[tid.x] + right) / 3.0;
}
```

**Without barrier:** Race condition! Thread might read before neighbor writes.

#### What Boon Has

```boon
-- FLUSH is completely different (early exit, not synchronization)
result: value |> WHEN {
    Ok[v] => v
    error => FLUSH { error }  -- Exit expression early
}
```

**FLUSH semantics:**
- Exits current expression
- Bypasses functions
- Propagates to boundaries
- **Temporal** (happens over time in reactive flow)

**NOT a synchronization primitive for parallel threads!**

#### What Boon Lacks

- No **barrier** concept (wait for all threads)
- No **memory fences** (ensure write visibility)
- No **synchronization** between parallel executions
- FLUSH is for error handling, not parallel coordination

#### Why This Matters

**Without barriers, you cannot:**
- Safely share data between threads
- Implement parallel reductions
- Do multi-phase algorithms
- Coordinate work across threads

**Every GPU parallel algorithm needs barriers:**
- Matrix multiply (tiled)
- FFT
- Scan/prefix sum
- Histogram
- Image filters (convolution)

#### What Would Be Needed

```boon
-- Barrier primitives
BARRIER_WORKGROUP   -- Wait for all threads in work group
BARRIER_STORAGE     -- Ensure storage writes visible
BARRIER_TEXTURE     -- Ensure texture writes visible

-- Example usage
shared_data[thread_id] = my_value
BARRIER_WORKGROUP  -- Wait here
neighbor_value: shared_data[thread_id + 1]  -- Safe to read
```

---

### 4. Atomic Operations ⚠️ CRITICAL GAP

#### What GPUs Have

```wgsl
// Atomic operations for safe concurrent modification
atomicAdd(&counter, 1u);              // Atomic increment
atomicMax(&max_value, current);       // Atomic max
atomicMin(&min_value, current);       // Atomic min
atomicAnd(&flags, mask);              // Atomic bitwise AND
atomicOr(&flags, mask);               // Atomic bitwise OR
atomicXor(&flags, mask);              // Atomic bitwise XOR
atomicExchange(&value, new_value);    // Atomic swap
atomicCompareExchangeWeak(&lock, 0u, 1u);  // Compare-and-swap

// Example: Parallel histogram computation
var<storage, read_write> histogram: array<atomic<u32>, 256>;

@compute @workgroup_size(256)
fn compute_histogram(@builtin(global_invocation_id) tid: vec3<u32>) {
    for (var i = tid.x; i < count; i += total_threads) {
        let value = data[i];
        let bin = u32(value * 255.0);

        // Multiple threads can safely increment same bin!
        atomicAdd(&histogram[bin], 1u);
    }
}
```

**Without atomics:** Race condition! Lost updates!

```wgsl
// WRONG: Non-atomic increment
histogram[bin] = histogram[bin] + 1;  // RACE CONDITION!

// What happens:
// Thread A reads histogram[5] = 10
// Thread B reads histogram[5] = 10  (before A writes)
// Thread A writes histogram[5] = 11
// Thread B writes histogram[5] = 11  (overwrites A's update!)
// Result: Two increments, but value only increased by 1 (WRONG!)
```

#### What Boon Has

```boon
-- All operations are regular (non-atomic)
counter: counter + 1
max_value: Math/max(max_value, current)

-- No concept of:
-- - Concurrent modification
-- - Atomic read-modify-write
-- - Memory ordering
-- - Compare-and-swap
```

#### What Boon Lacks

- No **atomic operations** on shared data
- No **compare-and-swap** primitives
- No **memory ordering** controls (acquire/release semantics)
- No way to **safely modify shared state** from parallel threads

#### Why This Matters

**Without atomics, you cannot write:**

1. **Parallel Histogram**
```wgsl
// Each thread processes subset of data
// Multiple threads may need to increment same bin
atomicAdd(&histogram[bin], 1u);  // REQUIRED
```

2. **Parallel Global Reduction**
```wgsl
// Each work group computes partial sum
// Write to global counter
if (tid == 0) {
    atomicAdd(&global_sum, workgroup_sum);  // REQUIRED
}
```

3. **Lock-Free Data Structures**
```wgsl
// Concurrent queue, stack, etc.
atomicCompareExchangeWeak(&head, old, new);  // REQUIRED
```

4. **Collision Detection / Spatial Hashing**
```wgsl
// Multiple objects may hash to same cell
atomicAdd(&cell_count[hash], 1u);  // REQUIRED
```

5. **Parallel Scan (Prefix Sum)**
```wgsl
// Global synchronization via atomic flags
atomicAdd(&flags[workgroup_id], 1u);  // REQUIRED
```

#### What Would Be Needed

```boon
-- Atomic operations as primitives
counter |> Atomic/add(1)
max_val |> Atomic/max(current)
lock |> Atomic/compare_exchange(old: 0, new: 1)

-- Or method syntax
histogram[bin] |> Atomic/add(1)
global_sum |> Atomic/add(local_sum)

-- With memory ordering control
value |> Atomic/store(new_value, order: Release)
loaded: value |> Atomic/load(order: Acquire)
```

---

### 5. Derivative Functions (Gradient Operations) 🤯 MIND-BENDING GAP

#### What GPUs Have

```wgsl
// These access data from NEIGHBORING shader invocations!
fn fragment_shader(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    // Partial derivatives: rate of change between adjacent pixels
    let ddx_uv = dpdx(uv);  // Change in UV from pixel (x, y) to (x+1, y)
    let ddy_uv = dpdy(uv);  // Change in UV from pixel (x, y) to (x, y+1)

    // Combined gradient magnitude
    let gradient = fwidth(uv);  // sqrt(ddx² + ddy²)

    // Example: Automatic mipmap level selection
    let mip_level = log2(max(length(dpdx(uv)), length(dpdy(uv))));

    return textureSampleLevel(my_texture, my_sampler, uv, mip_level);
}
```

#### The Mind-Bending Part

**This accesses neighboring invocations' register values!**

```wgsl
// Fragment shader running on pixel (100, 200):
let my_uv = uv;           // vec2(0.5, 0.3) - my UV coordinate
let dx = dpdx(uv);        // Compares with pixel (101, 200)'s UV
let dy = dpdy(uv);        // Compares with pixel (100, 201)'s UV

// The GPU AUTOMATICALLY:
// 1. Groups fragments into 2×2 quads
// 2. Shares register values within quad
// 3. Computes differences
// All without explicit synchronization!
```

**Example:**
```
Quad of 4 pixels:
  Pixel (100, 200): uv = (0.50, 0.30)
  Pixel (101, 200): uv = (0.51, 0.30)  ← dpdx uses this
  Pixel (100, 201): uv = (0.50, 0.31)  ← dpdy uses this
  Pixel (101, 201): uv = (0.51, 0.31)

  dpdx(uv) = (0.51, 0.30) - (0.50, 0.30) = (0.01, 0.00)
  dpdy(uv) = (0.50, 0.31) - (0.50, 0.30) = (0.00, 0.01)
```

#### What Boon Has

```boon
-- Functions only see their own inputs
FUNCTION compute_color(uv) {
    -- Can access: uv (my parameter)
    -- Cannot access: neighbor's uv

    [r: uv.x, g: uv.y, b: 0.5, a: 1.0]
}
```

**Boon has no concept of:**
- "Neighboring invocation"
- Spatial derivatives
- Automatic register sharing between parallel threads

#### What Boon Lacks

- No **derivative functions** (`dpdx`, `dpdy`, `fwidth`)
- No **implicit quad grouping** (2×2 fragment groups)
- No **cross-invocation register access**
- No **automatic gradient computation**

#### Why This Matters

**Derivatives are essential for:**

1. **Texture Mipmap Selection**
```wgsl
// GPU automatically picks correct mip level based on UV gradient
let color = textureSample(tex, samp, uv);  // Uses dpdx/dpdy internally!
```

2. **Normal Mapping**
```wgsl
// Compute tangent-space basis from position derivatives
let dPdx = dpdx(world_pos);
let dPdy = dpdy(world_pos);
let dUVdx = dpdx(uv);
let dUVdy = dpdy(uv);
let tangent = normalize(dPdx * dUVdy.y - dPdy * dUVdx.y);
```

3. **Anti-Aliasing**
```wgsl
// Detect edges via gradient
let edge_strength = length(fwidth(color));
```

4. **Procedural Filtering**
```wgsl
// Filter procedural patterns to avoid aliasing
let checker = sin(uv.x * 100.0) * sin(uv.y * 100.0);
let filtered = checker / max(1.0, length(fwidth(uv)) * 100.0);
```

#### What Would Be Needed

```boon
-- Derivative primitives
FUNCTION fragment_shader(uv: Vec2) {
    uv_dx: DERIVATIVE_X(uv)  -- Access from neighboring thread (x+1)
    uv_dy: DERIVATIVE_Y(uv)  -- Access from neighboring thread (y+1)
    uv_gradient: DERIVATIVE_WIDTH(uv)  -- Combined gradient

    -- Use for mipmap level
    mip: log2(max(length(uv_dx), length(uv_dy)))

    texture_color: my_texture |> Texture/sample_level(uv, mip)
}
```

**Compiler would need to:**
- Group fragment shader invocations into 2×2 quads
- Share registers across quad
- Generate derivative instructions

---

### 6. Texture Sampling Hardware 🎨 SPECIALIZED HARDWARE GAP

#### What GPUs Have

```wgsl
@group(0) @binding(0) var my_texture: texture_2d<f32>;
@group(0) @binding(1) var my_sampler: sampler;

fn fragment_shader(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    // Simple sample: hardware does ALL of this:
    let color = textureSample(my_texture, my_sampler, uv);

    // What the GPU does automatically:
    // 1. Compute derivatives: dpdx(uv), dpdy(uv)
    // 2. Calculate mip level: log2(max(length(ddx), length(ddy)))
    // 3. Sample 4 texels from mip N (bilinear)
    // 4. Sample 4 texels from mip N+1 (bilinear)
    // 5. Trilinear interpolate between mip levels
    // 6. Apply addressing mode (wrap/clamp/mirror)
    // All in specialized hardware, extremely fast!

    // Explicit mip level
    let detailed = textureSampleLevel(my_texture, my_sampler, uv, 0.0);

    // Explicit gradient
    let custom = textureSampleGrad(my_texture, my_sampler, uv, ddx, ddy);

    // Texture array
    let layer = textureSample(texture_array, my_sampler, uv, layer_index);

    // Cube map
    let env = textureSample(skybox, my_sampler, direction);

    return color;
}

// Sampler configuration (created on CPU)
const sampler_desc = {
    addressModeU: 'repeat',
    addressModeV: 'clamp',
    magFilter: 'linear',
    minFilter: 'linear',
    mipmapFilter: 'linear',
    maxAnisotropy: 16,
};
```

#### Texture Features

**Texture Types:**
- `texture_2d<f32>` - 2D texture
- `texture_2d_array<f32>` - Array of 2D textures
- `texture_3d<f32>` - 3D volume texture
- `texture_cube<f32>` - Cube map (6 faces)
- `texture_depth_2d` - Depth texture

**Sampling Modes:**
- **Filtering:** Point, Linear, Anisotropic
- **Mipmap:** Nearest, Linear (trilinear)
- **Address:** Repeat, Clamp, Mirror, Border

**Special Functions:**
- `textureSample()` - Automatic mip selection
- `textureSampleLevel()` - Explicit mip level
- `textureSampleGrad()` - Explicit gradients
- `textureSampleCompare()` - Depth comparison (shadows)
- `textureLoad()` - Direct texel fetch (no filtering)
- `textureGather()` - Gather 2×2 texels

#### What Boon Has

```boon
-- Nothing for textures
-- Records with arrays could represent texture data, but:
image_data: [
    width: 1024
    height: 1024
    pixels: ARRAY { 1048576, Vec4 }  -- 1024×1024 pixels
]

-- Manual lookup (no filtering, no mipmaps, no hardware)
FUNCTION sample_texture(image, uv) {
    x: floor(uv.x * image.width)
    y: floor(uv.y * image.height)
    index: y * image.width + x
    image.pixels[index]  -- Point sampling only
}
```

**To do proper bilinear filtering manually:**
```boon
-- Would need ~50 lines of code
-- No hardware acceleration
-- Very slow
```

#### What Boon Lacks

- No **texture types** (`texture_2d`, `texture_cube`, etc.)
- No **sampler types** (filtering, addressing configuration)
- No **hardware sampling** operations
- No **mipmap** support
- No **filtering** (bilinear, trilinear, anisotropic)
- No **addressing modes** (wrap, clamp, mirror)
- No **texture arrays** or **cube maps**

#### Why This Matters

**Graphics code is 90% texture sampling:**

```wgsl
// Typical PBR material shader
let albedo = textureSample(albedo_map, samp, uv);
let normal_map = textureSample(normal_map, samp, uv);
let roughness = textureSample(roughness_map, samp, uv).r;
let metallic = textureSample(metallic_map, samp, uv).r;
let ao = textureSample(ao_map, samp, uv).r;
let emissive = textureSample(emissive_map, samp, uv);
```

**Without hardware texture sampling:** 100-1000x slower!

#### What Would Be Needed

```boon
-- Texture types
TEXTURE2D albedo_texture
TEXTURE_CUBE skybox_texture
TEXTURE3D volume_texture

-- Sampler configuration
SAMPLER linear_sampler: [
    filter: Linear
    mipmap: Linear
    address_u: Repeat
    address_v: Clamp
    max_anisotropy: 16
]

-- Sampling operations
albedo: albedo_texture
    |> Texture/sample(sampler: linear_sampler, uv: uv)

skybox_color: skybox_texture
    |> Texture/sample_cube(sampler: linear_sampler, direction: view_dir)

detailed: albedo_texture
    |> Texture/sample_level(sampler: linear_sampler, uv: uv, level: 0.0)
```

---

### 7. Vector Swizzling 🎯 SYNTACTIC POWER GAP

#### What GPUs Have

```wgsl
let v = vec4(1.0, 2.0, 3.0, 4.0);

// Component access
let x = v.x;           // 1.0
let w = v.w;           // 4.0

// Multi-component swizzles (NEW vector created)
let xy = v.xy;         // vec2(1.0, 2.0)
let rgb = v.xyz;       // vec3(1.0, 2.0, 3.0)
let rgba = v.xyzw;     // vec4(1.0, 2.0, 3.0, 4.0)

// Arbitrary reordering
let yx = v.yx;         // vec2(2.0, 1.0) - reversed!
let bgr = v.zyx;       // vec3(3.0, 2.0, 1.0) - reversed!
let bgra = v.zyxw;     // vec4(3.0, 2.0, 1.0, 4.0) - swapped!

// Duplication/broadcast
let xx = v.xx;         // vec2(1.0, 1.0)
let xxxx = v.xxxx;     // vec4(1.0, 1.0, 1.0, 1.0)
let xxyy = v.xxyy;     // vec4(1.0, 1.0, 2.0, 2.0)

// Write swizzles
var v = vec4(1.0, 2.0, 3.0, 4.0);
v.xy = vec2(5.0, 6.0);  // Now v = vec4(5.0, 6.0, 3.0, 4.0)
v.zw = v.xy;            // Now v = vec4(5.0, 6.0, 5.0, 6.0)

// Color/texture channel swizzling
let color_rgba = texture.rgba;
let color_bgra = texture.bgra;  // Swap R and B channels
let grayscale = color.rrr;      // Broadcast red channel
```

#### Real-World Usage

**Graphics code is full of swizzling:**

```wgsl
// Typical vertex shader
fn vertex_main(@location(0) pos: vec3<f32>) -> VertexOutput {
    let world_pos = (model_matrix * vec4(pos, 1.0)).xyz;
    let clip_pos = projection * view * vec4(world_pos, 1.0);

    return VertexOutput {
        position: clip_pos,
        world_position: world_pos,
        uv: pos.xy,  // Use X and Y as UV
    };
}

// Typical fragment shader
fn fragment_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let albedo = textureSample(albedo_map, samp, uv).rgb;
    let normal_map = textureSample(normal_map, samp, uv).xyz * 2.0 - 1.0;

    // Lighting (vectors are everywhere)
    let light_dir = normalize(light_pos.xyz - world_pos.xyz);
    let view_dir = normalize(camera_pos.xyz - world_pos.xyz);
    let half_vec = normalize(light_dir + view_dir);

    let ndotl = max(0.0, dot(normal.xyz, light_dir));
    let ndoth = max(0.0, dot(normal.xyz, half_vec));

    return vec4(albedo * ndotl, 1.0);  // RGB + alpha
}
```

**Without swizzling, would need:**
```wgsl
// Verbose version (NO swizzling)
let world_pos_vec3 = vec3(world_pos.x, world_pos.y, world_pos.z);
let light_pos_vec3 = vec3(light_pos.x, light_pos.y, light_pos.z);
let difference = vec3(
    light_pos_vec3.x - world_pos_vec3.x,
    light_pos_vec3.y - world_pos_vec3.y,
    light_pos_vec3.z - world_pos_vec3.z
);
// ... 10x more verbose!
```

#### What Boon Has

```boon
-- Records with field access
color: [r: 1.0, g: 0.5, b: 0.2, a: 1.0]
r: color.r  -- Single field access: OK
g: color.g

-- But cannot do:
-- rgb: color.rgb  -- Error: .rgb is not a field
-- bgra: color.bgra  -- Error: .bgra is not a field
-- xxx: color.rrr  -- Error: .rrr is not a field

-- Would need to manually construct:
rgb: [r: color.r, g: color.g, b: color.b]  -- Verbose!
bgra: [b: color.b, g: color.g, r: color.r, a: color.a]  -- Very verbose!
```

#### What Boon Lacks

- No **vector types** with component names (x, y, z, w or r, g, b, a)
- No **swizzle syntax** (`v.xyz`, `v.xxyy`, `v.bgra`)
- No **implicit vector construction** from swizzles
- Records are close but lack swizzle notation

#### Why This Matters

**Swizzling makes graphics code readable:**

```wgsl
// Concise and clear
let direction = normalize(target.xyz - origin.xyz);
let color_rgb = texture.rgb;
let flipped = color.bgr;

// vs Boon equivalent (very verbose)
let direction = normalize([
    x: target.x - origin.x,
    y: target.y - origin.y,
    z: target.z - origin.z
])
let color_rgb = [r: texture.r, g: texture.g, b: texture.b]
let flipped = [r: color.b, g: color.g, b: color.r]
```

#### What Would Be Needed

```boon
-- Vector types with swizzle support
v: Vec4 { x: 1.0, y: 2.0, z: 3.0, w: 4.0 }

-- Component swizzles
xy: v.xy        -- Vec2 { x: 1.0, y: 2.0 }
rgb: v.xyz      -- Vec3 { x: 1.0, y: 2.0, z: 3.0 }
bgra: v.zyxw    -- Vec4 { x: 3.0, y: 2.0, z: 1.0, w: 4.0 }

-- Duplication
xxxx: v.xxxx    -- Vec4 { x: 1.0, y: 1.0, z: 1.0, w: 1.0 }

-- Write swizzles
v.xy = Vec2 { x: 5.0, y: 6.0 }  -- Now v = Vec4 { x: 5.0, y: 6.0, z: 3.0, w: 4.0 }
```

**Compiler would need:**
- Recognize swizzle patterns (`xyz`, `bgra`, `xxxx`, etc.)
- Generate code for component extraction/rearrangement
- Support both read and write swizzles

---

### 8. Interpolation Qualifiers 📐 RASTERIZATION CONTROL GAP

#### What GPUs Have

```wgsl
// Vertex shader outputs
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,

    // Different interpolation modes for different attributes
    @location(0) @interpolate(perspective, center) color: vec4<f32>,
    @location(1) @interpolate(linear) depth: f32,
    @location(2) @interpolate(flat) triangle_id: u32,
    @location(3) @interpolate(perspective, centroid) uv: vec2<f32>,
}

@vertex
fn vertex_main(@location(0) position: vec3<f32>) -> VertexOutput {
    return VertexOutput {
        clip_position: transform(position),
        color: vec4(1.0, 0.0, 0.0, 1.0),  // Red vertex
        depth: position.z,
        triangle_id: 42,
        uv: vec2(0.0, 0.0),
    };
}

// Fragment shader receives interpolated values
@fragment
fn fragment_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // in.color: Perspective-correct interpolated between vertices
    // in.depth: Linear interpolation (screen space)
    // in.triangle_id: Flat (no interpolation, same for whole triangle)
    // in.uv: Perspective-correct, centroid sampling (multisampling-aware)

    return in.color;
}
```

#### Interpolation Modes

**1. `@interpolate(perspective, center)` - Default**
```
Perspective-correct interpolation, sampled at pixel center

Example: Color varying across triangle
  Vertex 0: color = (1, 0, 0)  // Red
  Vertex 1: color = (0, 1, 0)  // Green
  Vertex 2: color = (0, 0, 1)  // Blue

  Fragment at triangle center: color ≈ (0.33, 0.33, 0.33) // Gray
```

**2. `@interpolate(linear)` - Screen-space linear**
```
Linear in screen space (not perspective-correct)
Used for: Depth values, screen-space effects

Perspective-correct: Correct for 3D attributes (UVs, normals, colors)
Linear: Faster, used for screen-space quantities
```

**3. `@interpolate(flat)` - No interpolation**
```
Same value for entire triangle (from provoking vertex)
Used for: IDs, discrete values

  Vertex 0: triangle_id = 42
  Vertex 1: triangle_id = 42
  Vertex 2: triangle_id = 42

  All fragments: triangle_id = 42  (no interpolation)
```

**4. `@interpolate(perspective, centroid)` - Multisampling**
```
Samples at centroid of covered samples (not pixel center)
Used with MSAA to avoid artifacts at triangle edges

Regular: Sample at pixel center (may be outside triangle)
Centroid: Sample at centroid of covered samples (always inside)
```

#### What Boon Has

```boon
-- Functions transform inputs to outputs
FUNCTION vertex_transform(position) {
    [
        clip_pos: transform(position)
        color: [r: 1.0, g: 0.0, b: 0.0, a: 1.0]
    ]
}

-- But no concept of:
-- - How these values are interpolated across triangle
-- - Rasterization
-- - Fixed-function pipeline stages
```

#### What Boon Lacks

- No **interpolation qualifiers** (perspective, linear, flat)
- No **sample location control** (center, centroid)
- No concept of **rasterization** (converting triangles to pixels)
- No **fixed-function pipeline integration**

#### Why This Matters

**Wrong interpolation = visual artifacts:**

```wgsl
// WRONG: Flat interpolation for color
@location(0) @interpolate(flat) color: vec4<f32>

// Result: Solid color triangle (no gradient)
// Vertex colors ignored except for first vertex
```

```wgsl
// WRONG: Linear interpolation for UVs
@location(1) @interpolate(linear) uv: vec2<f32>

// Result: Texture warping/distortion
// Perspective foreshortening not accounted for
```

```wgsl
// WRONG: Perspective interpolation for triangle ID
@location(2) @interpolate(perspective) triangle_id: u32

// Result: Undefined behavior (interpolating integer as float)
```

#### What Would Be Needed

```boon
-- Shader stage outputs with interpolation control
VERTEX_OUTPUT {
    position: BUILTIN  -- Special: clip-space position

    color: LOCATION(0, interpolate: Perspective, sample: Center)
    depth: LOCATION(1, interpolate: Linear)
    id: LOCATION(2, interpolate: Flat)
    uv: LOCATION(3, interpolate: Perspective, sample: Centroid)
}

FUNCTION vertex_main(pos: Vec3) -> VERTEX_OUTPUT {
    [
        position: transform(pos)
        color: vertex_color
        depth: pos.z
        id: 42
        uv: vertex_uv
    ]
}
```

---

### 9. Subgroup/Wave Operations 🌊 SIMD COMMUNICATION GAP

#### What GPUs Have

```wgsl
// Enable subgroup operations
enable subgroups;

@compute @workgroup_size(256)
fn compute_main(@builtin(subgroup_invocation_id) lane_id: u32) {
    let my_value = data[global_id];

    // Read from another lane in same wave (32-64 threads)
    let other_value = subgroupShuffle(my_value, 5);  // Read lane 5's value

    // Vote operations (all threads in wave)
    let all_positive = subgroupAll(my_value > 0);    // True if ALL > 0
    let any_negative = subgroupAny(my_value < 0);    // True if ANY < 0

    // Parallel reduction within wave
    let wave_sum = subgroupAdd(my_value);     // Sum of all lanes
    let wave_max = subgroupMax(my_value);     // Max of all lanes
    let wave_min = subgroupMin(my_value);     // Min of all lanes
    let wave_and = subgroupAnd(flags);        // Bitwise AND

    // Broadcast operations
    let first_value = subgroupBroadcast(my_value, 0);  // Lane 0 to all

    // Elect first active lane
    if (subgroupElect()) {
        // Only first lane in wave executes this
    }

    // Shuffle operations
    let next_lane = subgroupShuffleUp(my_value, 1);    // Get from lane-1
    let prev_lane = subgroupShuffleDown(my_value, 1);  // Get from lane+1
    let xor_lane = subgroupShuffleXor(my_value, 1);    // XOR with lane index
}
```

#### Real-World Example: Fast Reduction

```wgsl
// Reduce 1 million values to single sum
var<workgroup> partial_sums: array<atomic<u32>, 8>;  // One per wave

@compute @workgroup_size(256)
fn parallel_sum(@builtin(global_invocation_id) gid: vec3<u32>,
                @builtin(subgroup_invocation_id) lane: u32) {
    // Each thread loads one value
    var sum = data[gid.x];

    // Wave-level reduction (32-64 threads, NO synchronization needed!)
    sum = subgroupAdd(sum);  // All lanes reduced in hardware

    // Only first lane in each wave writes (256/32 = 8 writes)
    if (subgroupElect()) {
        let wave_id = gid.x / 32;
        atomicAdd(&partial_sums[wave_id], sum);
    }

    workgroupBarrier();

    // Final reduction across 8 partial sums (single wave)
    if (gid.x < 8) {
        let final = atomicLoad(&partial_sums[gid.x]);
        let total = subgroupAdd(final);
        if (subgroupElect() && gid.x == 0) {
            output[workgroup_id.x] = total;
        }
    }
}
```

**Performance:**
- Without subgroups: 256 threads → 256 atomic adds → SLOW
- With subgroups: 256 threads → 8 wave reductions + 8 atomic adds → 32x FASTER!

#### What Boon Has

```boon
-- LINK is for TEMPORAL reactive channels (events over time)
button_element() |> LINK { store.elements.button }

-- Later, consume events:
store.elements.button.event.press |> THEN { increment() }

-- This is temporal coordination (event flows)
-- NOT spatial coordination (parallel thread communication)
```

**LINK semantics:**
- Reactive channel for events
- Temporal (happens over time)
- One producer → multiple consumers
- For UI events, dataflow

**NOT for parallel GPU threads!**

#### What Boon Lacks

- No **subgroup/wave** concept (SIMD group of 32-64 threads)
- No **cross-lane shuffle** operations
- No **wave-level reductions** (parallel within SIMD)
- No **vote operations** (all/any across lanes)
- No **subgroup synchronization** (implicit within wave)
- LINK is temporal, not spatial

#### Why This Matters

**Subgroup ops enable:**

1. **Fast Reductions** (32x faster than atomics)
```wgsl
let wave_sum = subgroupAdd(my_value);  // One instruction!
```

2. **Cross-Lane Communication**
```wgsl
// Implement wave-level prefix sum
var prefix = my_value;
for (var i = 1; i < waveSize; i *= 2) {
    let prev = subgroupShuffleUp(prefix, i);
    if (lane >= i) { prefix += prev; }
}
```

3. **Divergence Optimization**
```wgsl
// Skip work if all threads agree
if (subgroupAll(skip_condition)) {
    return;  // Entire wave skips, no divergence!
}
```

4. **Occupancy Tracking**
```wgsl
let active_count = subgroupBallot(is_active).count();  // How many lanes active?
```

#### What Would Be Needed

```boon
-- Subgroup operations as primitives
BUILTIN subgroup_lane_id: U32
BUILTIN subgroup_size: U32

-- Shuffle operations
other_lane: SUBGROUP_SHUFFLE(my_value, lane: 5)
next: SUBGROUP_SHUFFLE_UP(my_value, delta: 1)
prev: SUBGROUP_SHUFFLE_DOWN(my_value, delta: 1)

-- Reduction operations
wave_sum: SUBGROUP_ADD(my_value)
wave_max: SUBGROUP_MAX(my_value)

-- Vote operations
all_pass: SUBGROUP_ALL(condition)
any_pass: SUBGROUP_ANY(condition)

-- Elect first active lane
is_first: SUBGROUP_ELECT  -- Bool
```

---

### 10. Built-in Pipeline Integration 🎬 FIXED-FUNCTION HARDWARE GAP

#### What GPUs Have

```wgsl
// Fragment shader just outputs color
@fragment
fn fragment_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let color = compute_color(uv);
    return color;  // GPU does the rest automatically!
}

// After fragment shader, GPU hardware automatically:
// 1. Depth test: Compare fragment depth with depth buffer
//    - If fragment is behind existing geometry: DISCARD
//    - If fragment is in front: CONTINUE
// 2. Stencil test: Check stencil buffer value
//    - Used for masking, shadows, portals
// 3. Blending: Combine with existing framebuffer color
//    - Alpha blending: src.a * src + (1 - src.a) * dst
//    - Additive: src + dst
//    - Multiplicative: src * dst
//    - Custom blend equations
// 4. Format conversion: Convert to framebuffer format
//    - RGBA8 (8 bits per channel)
//    - RGBA16F (16-bit float per channel)
//    - Etc.
// 5. Multisampling: Resolve MSAA samples
// 6. Dithering: Reduce banding artifacts
```

#### Pipeline Configuration (External to Shader)

```javascript
// WebGPU pipeline configuration
const pipeline = device.createRenderPipeline({
    // Shader modules
    vertex: {
        module: vertexShaderModule,
        entryPoint: 'vertex_main',
    },
    fragment: {
        module: fragmentShaderModule,
        entryPoint: 'fragment_main',
        targets: [{
            format: 'bgra8unorm',
            blend: {
                color: {
                    srcFactor: 'src-alpha',
                    dstFactor: 'one-minus-src-alpha',
                    operation: 'add',
                },
                alpha: {
                    srcFactor: 'one',
                    dstFactor: 'zero',
                    operation: 'add',
                },
            },
        }],
    },

    // Depth/stencil configuration
    depthStencil: {
        format: 'depth24plus',
        depthWriteEnabled: true,
        depthCompare: 'less',  // Fragment passes if depth < existing
    },

    // Multisampling
    multisample: {
        count: 4,  // 4x MSAA
    },

    // Primitive topology
    primitive: {
        topology: 'triangle-list',
        cullMode: 'back',
        frontFace: 'ccw',
    },
});
```

#### What Boon Has

```boon
-- Just functions that compute outputs
FUNCTION fragment_shader(uv) {
    color: compute_color(uv)
    color  -- Return color
}

-- No concept of:
-- - What happens after this function returns
-- - Depth testing
-- - Blending with framebuffer
-- - Format conversion
-- - Pipeline configuration
```

#### What Boon Lacks

- No **depth test** configuration (less, greater, equal, etc.)
- No **stencil operations** (masking, shadows)
- No **blending modes** (alpha, additive, multiplicative)
- No **multisampling** control (MSAA)
- No **primitive topology** (triangles, lines, points)
- No **culling mode** (back-face, front-face, none)
- All of this is external to shaders in WebGPU

#### Why This Matters

**Example: Transparency requires blending**

```wgsl
// Fragment shader outputs transparent color
@fragment
fn glass_shader() -> @location(0) vec4<f32> {
    return vec4(0.5, 0.8, 1.0, 0.3);  // 30% opaque blue glass
}

// Pipeline MUST configure alpha blending:
// blend.color.srcFactor = 'src-alpha'        (0.3)
// blend.color.dstFactor = 'one-minus-src-alpha' (0.7)
// Result: 0.3 * glass_color + 0.7 * background_color
```

**Without blending configuration:** Glass would be opaque!

**Example: 3D scene requires depth testing**

```wgsl
// Fragment shader computes color
@fragment
fn scene_shader() -> @location(0) vec4<f32> {
    return compute_lighting();
}

// Pipeline MUST configure depth test:
// depthCompare = 'less'
// Result: Only draws fragments closer than existing geometry
```

**Without depth test:** Distant objects would draw over near objects!

#### What Would Be Needed

```boon
-- Pipeline configuration (external to shader functions?)
RENDER_PIPELINE {
    vertex: vertex_main
    fragment: fragment_main

    depth_stencil: [
        format: Depth24Plus
        depth_write: True
        depth_compare: Less
    ]

    blend: [
        color: [
            src_factor: SrcAlpha
            dst_factor: OneMinusSrcAlpha
            operation: Add
        ]
    ]

    multisample: [count: 4]

    primitive: [
        topology: TriangleList
        cull_mode: Back
    ]
}
```

**Or:** Keep pipeline config external (like current WebGPU), only shaders in Boon.

---

## Fundamental Paradigm Differences

### Execution Model

```
Boon:    Sequential or Reactive (temporal)
         - Events flow through time
         - State updates over time
         - LATEST for reactive state
         - LINK for event channels

GPU:     Massively Parallel (spatial)
         - Same code, different data, same instant
         - Thousands of invocations simultaneously
         - Each invocation independent
         - Coordination via barriers/atomics
```

### Memory Model

```
Boon:    Flat namespace
         - Variables just exist "somewhere"
         - No hierarchy
         - No performance distinctions

GPU:     Hierarchical (performance-critical)
         - Private (registers): 1x latency
         - Workgroup (shared): 10x latency
         - Uniform (constant): 100x latency
         - Storage (global): 400x latency
         - Texture (specialized): 100x latency, massive bandwidth
```

### Communication

```
Boon:    LINK for reactive channels (temporal)
         - Events flowing over time
         - Producer → multiple consumers
         - Temporal coordination

GPU:     Multiple spatial mechanisms
         - Derivatives: Access neighboring fragment registers
         - Subgroups: Cross-lane shuffle within SIMD wave
         - Shared memory: Explicit sharing within work group
         - Atomics: Safe concurrent modification
         - All spatial, not temporal!
```

### Synchronization

```
Boon:    FLUSH for early exit
         - Exit expression
         - Bypass functions
         - Propagate to boundaries
         - Temporal (error handling)

GPU:     Barriers for parallel coordination
         - Wait for all threads in work group
         - Ensure memory visibility
         - Spatial (synchronization)
         - Not related to FLUSH semantics!
```

---

## The Ironic Reversal

### Boon → HDL: Accidental Success ✅

From [ACCIDENTALLY_MOTIVATING_REVIEW.md](../../playground/frontend/src/examples/hw_examples/hdl_analysis/ACCIDENTALLY_MOTIVATING_REVIEW.md):

> **Boon isn't becoming an HDL - it already is one**
>
> You designed these primitives for elegant software:
> - LATEST → Reactive state management
> - PASSED → Ambient context propagation
> - LINK → Bidirectional reactive channels
> - FLUSH → Early exit with bypass propagation
>
> But these same primitives *perfectly* describe hardware:
> - LATEST → Hardware registers (clock-triggered state)
> - PASSED → Clock/reset signals (ambient in hierarchy)
> - LINK → Wire connections (signal propagation)
> - FLUSH → Pipeline flush/stall (bypass logic)
>
> **~85% of HDL features emerged naturally from reactive abstractions.**

**Why it worked:** Hardware has **temporal nature** (clock cycles)
- Registers hold state over time ✅ LATEST
- Clocks propagate through hierarchy ✅ PASSED
- Wires connect components ✅ LINK
- Pipelines have temporal stages ✅ LATEST chains

### Boon → GPU: Fundamental Mismatch ❌

**For GPU shaders, the opposite is true:**
> "Boon would need to ADD spatial parallelism primitives that don't exist in its reactive temporal model"

**Why it doesn't work:** GPU has **spatial nature** (parallel threads)
- No temporal state (each invocation independent) ❌ LATEST not applicable
- No temporal events (no clock) ❌ PASSED.clk not applicable
- No temporal channels (threads don't stream events) ❌ LINK not applicable
- Spatial coordination (barriers, atomics) ❌ Completely missing
- Memory hierarchy (address spaces) ❌ Completely missing

**What's missing:**
- Work groups, thread IDs ❌ No parallelism model
- Memory address spaces ❌ No hierarchy
- Barriers, atomics ❌ No synchronization
- Derivatives, subgroups ❌ No spatial communication
- Textures, interpolation ❌ No hardware integration

---

## What Would Need to Be Added

### Tier 1: Essential (Can't write shaders without these) 🔴

**1. Parallelism Model**
```boon
COMPUTE[workgroup: 16, 16, 1]
FUNCTION process(
    BUILTIN global_id: Vec3U32,
    BUILTIN local_id: Vec3U32,
    BUILTIN workgroup_id: Vec3U32
) {
    -- Compiler knows this runs massively parallel
}
```

**2. Memory Address Spaces**
```boon
SHARED scratch: ARRAY { 256, Float32 }      -- Workgroup shared memory
UNIFORM params: CameraParams                 -- Read-only uniform
STORAGE particles: ARRAY { 10000, Particle } -- Global read-write
PRIVATE temp: Float32                        -- Thread-local
```

**3. Synchronization Primitives**
```boon
scratch[local_id] = compute_value()
BARRIER_WORKGROUP  -- Wait for all threads
neighbor: scratch[local_id + 1]  -- Safe: all writes done
```

**4. Atomic Operations**
```boon
histogram[bin] |> Atomic/add(1)
global_max |> Atomic/max(local_max)
lock |> Atomic/compare_exchange(old: 0, new: 1)
```

**5. Binding/Location Annotations**
```boon
UNIFORM[group: 0, binding: 0] camera: Camera
STORAGE[group: 0, binding: 1] output: ARRAY { 1000, Vec4 }
TEXTURE2D[group: 1, binding: 0] albedo
SAMPLER[group: 1, binding: 1] linear_sampler
```

### Tier 2: Important (Key functionality) 🟡

**6. Vector Types with Swizzling**
```boon
v: Vec4 { 1.0, 2.0, 3.0, 4.0 }
xy: v.xy        -- Vec2 { 1.0, 2.0 }
bgra: v.zyxw    -- Vec4 { 3.0, 2.0, 1.0, 4.0 }
xxxx: v.xxxx    -- Vec4 { 1.0, 1.0, 1.0, 1.0 }
```

**7. Texture Sampling**
```boon
TEXTURE2D albedo_map
SAMPLER linear_sampler: [
    filter: Linear
    mipmap: Linear
    address_u: Repeat
]

color: albedo_map |> Texture/sample(linear_sampler, uv)
detailed: albedo_map |> Texture/sample_level(linear_sampler, uv, 0.0)
```

**8. Derivative Functions**
```boon
uv_dx: DERIVATIVE_X(uv)  -- dpdx(uv)
uv_dy: DERIVATIVE_Y(uv)  -- dpdy(uv)
grad: DERIVATIVE_WIDTH(uv)  -- fwidth(uv)

-- Use for mipmap selection
mip_level: log2(max(length(uv_dx), length(uv_dy)))
```

**9. Matrix Types**
```boon
Mat4: [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0]
]

transformed: matrix * vector  -- Matrix-vector multiply
combined: matrix_a * matrix_b  -- Matrix-matrix multiply
```

### Tier 3: Advanced (Optimization) 🟢

**10. Subgroup Operations**
```boon
wave_sum: SUBGROUP_ADD(my_value)
other_lane: SUBGROUP_SHUFFLE(value, lane: 5)
all_positive: SUBGROUP_ALL(condition)
is_first: SUBGROUP_ELECT
```

**11. Interpolation Qualifiers**
```boon
VERTEX_OUTPUT {
    position: BUILTIN
    color: LOCATION(0, interpolate: Perspective)
    depth: LOCATION(1, interpolate: Linear)
    id: LOCATION(2, interpolate: Flat)
}
```

**12. Shader Stage Declarations**
```boon
VERTEX_SHADER vertex_main(BUILTIN vertex_id: U32) {
    -- Vertex processing
}

FRAGMENT_SHADER fragment_main(
    BUILTIN position: Vec4,
    LOCATION(0) color: Vec4
) {
    -- Fragment processing
}

COMPUTE_SHADER[workgroup: 256, 1, 1] compute_main(
    BUILTIN global_id: Vec3U32
) {
    -- Compute processing
}
```

---

## Comparison Table: What Boon Can/Cannot Express

| GPU Capability | Boon Equivalent | Status | Gap Severity |
|----------------|-----------------|--------|--------------|
| **Work groups** | None | ❌ Missing | 🔴 CRITICAL |
| **Thread IDs** | None | ❌ Missing | 🔴 CRITICAL |
| **Memory spaces** | Flat variables | ❌ Missing | 🔴 CRITICAL |
| **Barriers** | None (FLUSH is different) | ❌ Missing | 🔴 CRITICAL |
| **Atomics** | None | ❌ Missing | 🔴 CRITICAL |
| **Derivatives** | None | ❌ Missing | 🟡 MAJOR |
| **Texture sampling** | None | ❌ Missing | 🟡 MAJOR |
| **Vector swizzling** | Records (partial) | ⚠️ Partial | 🟡 MAJOR |
| **Interpolation** | None | ❌ Missing | 🟠 MODERATE |
| **Subgroup ops** | LINK (temporal, not spatial) | ❌ Missing | 🟠 MODERATE |
| **Pipeline config** | None | ❌ Missing | 🟢 MINOR |
| **Pure functions** | ✅ Full support | ✅ Works | ✅ Compatible |
| **Pattern matching** | ✅ WHEN/WHILE | ✅ Works | ✅ Compatible |
| **BITS operations** | ✅ Full support | ✅ Works | ✅ Compatible |
| **Records** | ✅ Full support | ✅ Works | ✅ Compatible |
| **Fixed arrays** | ✅ ARRAY | ✅ Works | ✅ Compatible |

---

## Final Assessment

### Can Boon Express GPU Shaders?

**Answer by component:**

| Component | Assessment |
|-----------|------------|
| **Algorithms (math, logic)** | ✅ YES (~60%) - Pure functions, pattern matching, BITS ops work great |
| **Parallel execution model** | ❌ NO - Work groups, thread IDs completely missing |
| **Memory hierarchy** | ❌ NO - Needs address spaces (private, shared, uniform, storage) |
| **Thread coordination** | ❌ NO - Needs barriers and atomics |
| **Hardware features** | ❌ NO - Needs textures, derivatives, interpolation, subgroups |

### The Bottom Line

**What GPU shaders do that Boon cannot express:**

**THE ENTIRE PARALLEL EXECUTION MODEL THAT MAKES GPUs WHAT THEY ARE.**

Boon could express:
- ✅ The **algorithms** (compute color, transform vertex, apply filter)
- ✅ The **math** (vector ops, matrix multiply, lighting equations)
- ✅ The **logic** (pattern matching, conditionals)

Boon cannot express:
- ❌ The **parallelism** (work groups, threads, SIMD)
- ❌ The **memory management** (address spaces, hierarchies)
- ❌ The **coordination** (barriers, atomics, synchronization)
- ❌ The **hardware integration** (textures, rasterization, derivatives)

### What Would It Take?

**To add all these features would essentially be building a new language on top of Boon:**

A "Boon-GPU" dialect with:
- Different execution model (spatial parallelism vs temporal reactivity)
- Different memory model (hierarchical vs flat)
- Different communication model (spatial vs temporal)
- Different primitives (barriers vs FLUSH, atomics vs regular ops)

**Estimated effort:** ~40% of a new language
- Core functional subset: Already works
- Parallelism model: New (~10%)
- Memory model: New (~10%)
- Synchronization: New (~5%)
- Hardware features: New (~15%)

---

## Philosophical Conclusion

### The Duality of Computation

**Boon discovered a profound truth:**
> Reactive programming naturally describes **temporal computation** (events flowing through time)

**This research reveals the complement:**
> GPU shaders naturally describe **spatial computation** (data flowing through space)

**Two fundamental models:**
- **Temporal:** State, events, reactivity → UI, Hardware, Databases
- **Spatial:** Parallelism, SIMD, coordination → GPUs, HPC, Neural Networks

**Boon mastered one, but not the other.**

### The Accidental HDL vs The Impossible GPU

**Why Boon → HDL worked:**
- Hardware circuits are **temporal** (clock cycles, state transitions)
- Reactive abstractions **naturally map** to clocked circuits
- LATEST, PASSED, LINK describe temporal hardware perfectly
- **~85% emergence** from existing features

**Why Boon → GPU doesn't work:**
- GPU shaders are **spatial** (parallel threads, simultaneous execution)
- Reactive abstractions **don't map** to spatial parallelism
- Missing: work groups, barriers, atomics, memory spaces
- **~40% of features would need to be added**

### The Insight

**Boon didn't just "become an HDL by accident."**

Boon found the **universal abstraction for temporal computation:**
- Reactive values
- Flowing through channels
- Transforming over time
- With explicit dependencies

**This abstraction works for anything temporal:**
- UI events ✅
- Hardware clocks ✅
- Database streams ✅
- Network flows ✅
- GPU shaders ❌ (spatial, not temporal)

**GPUs need the dual abstraction for spatial computation:**
- Parallel threads
- Executing simultaneously
- Coordinating through barriers
- With explicit memory hierarchies

**Different fundamental model.**

---

## Recommendations

### Option 1: Don't Target GPU Shaders ✅ RECOMMENDED

**Rationale:**
- Core paradigm mismatch (temporal vs spatial)
- Would require ~40% new language features
- Would create "Boon-GPU" dialect, fragmenting the language
- Better to focus on Boon's strengths (UI, hardware, databases)

**Keep Boon for:**
- Web applications ✅
- Hardware (FPGA/ASIC) ✅
- Databases/streams ✅
- CLI tools ✅

**Use existing shader languages for GPUs:**
- WGSL for WebGPU
- GLSL for OpenGL
- HLSL for DirectX

### Option 2: Limited Compute Shader Support ⚠️ POSSIBLE

**If you must target GPUs:**
- Focus on **compute shaders only** (not vertex/fragment)
- Add minimal parallelism support (work groups, thread IDs)
- Add memory spaces and barriers
- Skip advanced features (derivatives, interpolation, rasterization)

**Would enable:**
- Data-parallel algorithms ✅
- Image processing ✅
- Particle simulation ✅
- Scientific computing ✅

**Would NOT enable:**
- 3D graphics rendering ❌
- Texture-heavy shaders ❌
- Rasterization pipeline ❌

**Effort:** ~20% of new features (vs ~40% for full support)

### Option 3: Research Contribution 🎓 INTERESTING

**Academic angle:**
> "Temporal vs Spatial Computation: Why Reactive Programming Describes Hardware But Not GPUs"

**Contributions:**
1. Identify fundamental dichotomy (temporal vs spatial)
2. Show reactive abstractions map to temporal domains
3. Demonstrate gap for spatial domains (this document!)
4. Propose dual abstractions for both

**Venues:**
- PLDI (Programming Languages)
- PPoPP (Parallel Programming)
- ASPLOS (Architecture/Languages)

---

## Appendix: Quick Reference

### Core Correspondences

**What Boon Has (Temporal):**
| Boon Primitive | Software | Hardware | GPU Shaders |
|----------------|----------|----------|-------------|
| LATEST | UI state | Register | ❌ No equivalent |
| PASSED | Parent context | Clock domain | ❌ No equivalent |
| LINK | Event channel | Wire | ❌ Not spatial |
| FLUSH | Error exit | Pipeline flush | ❌ Different semantics |
| PULSES | Iteration | Clock cycles | ⚠️ Could be loop |

**What GPU Has (Spatial):**
| GPU Feature | Boon Equivalent | Status |
|-------------|-----------------|--------|
| Work groups | ❌ None | Missing |
| Thread IDs | ❌ None | Missing |
| Memory spaces | ❌ Flat model | Missing |
| Barriers | ❌ None | Missing |
| Atomics | ❌ None | Missing |
| Derivatives | ❌ None | Missing |
| Textures | ❌ None | Missing |
| Subgroups | ❌ None | Missing |

---

**Research Date:** 2025-11-20
**Analysis Duration:** Deep investigation session
**Key Finding:** GPU shaders require **spatial parallelism primitives** that don't exist in Boon's **temporal reactive model**

**Status:** Complete analysis of fundamental paradigm mismatch 🔍
