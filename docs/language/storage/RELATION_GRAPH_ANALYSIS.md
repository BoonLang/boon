# RELATION & GRAPH Analysis: Beyond Tables

**Date**: 2025-01-20
**Status**: Research & Design Exploration
**Scope**: Relations, Graphs, Multi-Model Databases for Boon

---

## Executive Summary

After researching **Cell language**, **SurrealDB**, **Neo4j**, and comparing relational/graph/document models, key insights for Boon:

1. **Relations > Objects** for complex state (Cell's core argument)
2. **Graph edges** are natural for many-to-many relationships (SurrealDB, Neo4j)
3. **Multi-model** flexibility avoids choosing between paradigms (SurrealDB)
4. **Typed IDs** prevent entity confusion (Cell's innovation)
5. **Live queries** enable reactivity (SurrealDB feature)

**Recommendation**: Extend Boon with **RELATION** (Cell-inspired) + **GRAPH** (SurrealDB-inspired) alongside **TABLE**.

---

## Table of Contents

1. [Cell Language: Relations](#cell-language-relations)
2. [SurrealDB: Multi-Model Database](#surrealdb-multi-model-database)
3. [Graph Databases (Neo4j)](#graph-databases-neo4j)
4. [When to Use Each Model](#when-to-use-each-model)
5. [Proposal for Boon](#proposal-for-boon)
6. [Complete Examples](#complete-examples)
7. [Research Summary](#research-summary)

---

## Cell Language: Relations

### Core Philosophy

[**Cell**](https://www.cell-lang.net/) argues that **relations are superior to objects** for modeling complex application state.

**Key problems with objects:**
1. **Redundancy**: Bidirectional pointers must be manually synchronized
2. **Implementation pollution**: IDs stored redundantly (in object + dict key)
3. **Graph complexity**: Manual "wiring" between interconnected entities
4. **Pointer aliasing**: Complicates reactive programming

**Relations solve these:**
1. **No redundancy**: Single source of truth
2. **Omnidirectional navigation**: Query from any perspective
3. **Declarative integrity**: Keys, foreign keys built-in
4. **Value-based**: No pointer aliasing (perfect for reactive!)

### Syntax: Relations in Cell

#### **Unary Relations** (Sets)

```cell
// Entity existence
suppliers(SupplierId);
parts(PartId);

// Boolean properties
on_sale(PartId);
```

#### **Binary Relations** (Key-Value)

```cell
// Attributes with keys
name(SupplierId, String) [key: 0];    // Supplier → Name
address(SupplierId, String) [key: 0]; // Supplier → Address
price(PartId, Float) [key: 0];        // Part → Price
```

The `[key: 0]` constraint ensures uniqueness (like primary key).

#### **Ternary Relations** (Junction Tables)

```cell
// Supplier sells part at price
sells(SupplierId, PartId, Float);

// Employee works at company in role
employment(EmployeeId, CompanyId, Role);
```

**Why ternary?** Price depends on BOTH supplier AND part - can't be attribute of either alone!

#### **Symmetric Relations**

```cell
// Friendship (bidirectional)
are_friends(UserId | UserId);

// Equivalent to:
// are_friends(UserId, UserId);
// But enforces symmetry: if (A, B) exists, (B, A) also exists
```

### Querying Relations

#### **Omnidirectional Queries**

```cell
// Binary relation
usernames(UserId, String) [key: 0, key: 1];

// Forward: UserId → String
name: usernames(user_id);

// Reverse: String → UserId
id: usernames(!, "alice");

// Check existence
exists: usernames(user_id, _);  // Boolean

// Collect all
all_names: [name : id, name <- usernames];
```

**Same relation, multiple perspectives!**

#### **Ternary Queries**

```cell
sells(SupplierId, PartId, Float);

// Find price: supplier S sells part P at what price?
price: sells(supplier_s, part_p, ?);

// Find suppliers: who sells part P?
suppliers: [s : s, p, _ <- sells(?, part_p, ?)];

// Find parts: what does supplier S sell?
parts: [p : s, p, _ <- sells(supplier_s, ?, ?)];

// All sales with price > $100
expensive: [s, p : s, p, price <- sells, price > 100.0];
```

**Query from ANY argument!**

### Integrity Constraints

#### **Keys (Uniqueness)**

```cell
// Single key
usernames(UserId, String) [key: 0];  // UserId unique
emails(UserId, String) [key: 1];     // Email unique

// Composite key
employment(EmployeeId, CompanyId, Role)
  [key: 0, 1];  // (Employee, Company) unique
```

#### **Foreign Keys (Referential Integrity)**

```cell
// Mandatory: Every book must have author
written_by(BookId, AuthorId) -> books(BookId, _), authors(AuthorId, _);

// Existence constraint: Every book in 'books' must have entry in 'written_by'
books(BookId, _) -> written_by(BookId, _);
```

### Typed IDs (Preventing Confusion)

**Problem with plain integers:**

```cell
// ❌ Can't distinguish which entity type an ID represents
user_id: 42;
product_id: 42;
// Both are just integers - easy to mix up!
```

**Cell's solution: Custom ID types**

```cell
// Define typed IDs
type UserId = user_id(Nat);
type ProductId = product_id(Nat);
type CompanyId = company_id(Nat);

// Now these are different types!
user: user_id(42);      // Type: UserId
product: product_id(42); // Type: ProductId

// Compiler prevents mixing
users(UserId, String);
users(product, "Alice");  // ❌ Type error!
```

**Benefits:**
- Type safety (can't pass ProductId where UserId expected)
- Polymorphic relations (multiple `name` relations for different entity types)
- Clear intent (self-documenting)

### Performance

**Constant-time lookups** on any argument combination:

```cell
// All these are O(1):
usernames(user_id)           // Forward lookup
usernames(!, "alice")        // Reverse lookup
sells(supplier_s, part_p, ?) // Ternary lookup
```

Cell maintains **indexes automatically** for all query patterns.

### Separation of Schema and Code

**Schema:**

```cell
schema Suppliers {
  suppliers(SupplierId);
  name(SupplierId, String) [key: 0];
  sells(SupplierId, PartId, Float);
  sells(s, p, _) -> suppliers(s), parts(p);  // Foreign keys
}
```

**Code (separate):**

```cell
// Schema defined once, stable across changes
// Code evolves independently

add_supplier(id: SupplierId, name: String) {
  insert suppliers(id);
  insert name(id, name);
}

add_sale(supplier: SupplierId, part: PartId, price: Float) {
  // Foreign key constraint checked automatically!
  insert sells(supplier, part, price);
}
```

**Advantage:** Schema changes rarely; code changes frequently. Clean separation!

---

## SurrealDB: Multi-Model Database

[**SurrealDB**](https://surrealdb.com) combines **document**, **graph**, **relational**, **time-series**, **key-value**, and **geospatial** models in one database.

**Status 2025:** Production-ready, enterprise adoption growing.

### Graph Model

#### **Nodes and Edges**

```sql
-- Create nodes (tables)
CREATE person:alice SET name = "Alice", age = 30;
CREATE person:bob SET name = "Bob", age = 25;
CREATE post:hello SET title = "Hello World", content = "...";

-- Create edges (relationships)
RELATE person:alice->wrote->post:hello
  SET timestamp = time::now();

RELATE person:bob->likes->post:hello
  SET timestamp = time::now();

RELATE person:alice->friends_with->person:bob
  SET since = "2020-01-01";
```

**Key insight:** Edges are tables too! Can store properties.

#### **RELATE Syntax**

```sql
-- Basic: node -> edge -> node
RELATE person:alice->wrote->post:hello;

-- With properties on edge
RELATE person:alice->order->product:laptop
  SET quantity = 2, total = 2499.98;

-- Bidirectional (symmetric)
RELATE person:alice<->friends_with<->person:bob;
```

**Edges have IDs:**

```sql
-- Edge ID: person:alice->wrote->post:hello
SELECT * FROM ->wrote->;  -- All 'wrote' edges
```

#### **Graph Traversal**

```sql
-- Forward: Posts written by Alice
SELECT ->wrote->post FROM person:alice;

-- Backward: Author of post
SELECT <-wrote<-person FROM post:hello;

-- Bidirectional: Alice's friends
SELECT <->friends_with<->person FROM person:alice;

-- Multi-hop: Friends of friends
SELECT ->friends_with->person->friends_with->person
FROM person:alice;

-- Who liked Alice's posts?
SELECT ->wrote->post<-likes<-person FROM person:alice;
```

**Intuitive arrow syntax!**

#### **Recursive Traversal (v2.1+)**

```sql
-- All descendants (unlimited depth)
SELECT ->parent_of->{..+}->person FROM person:root;

-- Descendants up to 3 levels
SELECT ->parent_of->{..3}->person FROM person:root;

-- Shortest path
SELECT ->knows->{..+shortest=person:target}->person
FROM person:alice;
```

### Live Queries (Reactive!)

```sql
-- Subscribe to changes
LIVE SELECT * FROM person WHERE age > 18;

-- Returns a live query ID (UUID)
-- Server pushes updates when data changes!

-- Client receives notifications:
// INSERT: { id: person:charlie, age: 22 }
// UPDATE: { id: person:alice, age: 31 }
// DELETE: { id: person:bob }
```

**Perfect for reactive UIs!**

### Record Links vs Graph Edges

**Record Links** (like foreign keys):

```sql
CREATE author:alice SET name = "Alice";
CREATE book:moby SET
  title = "Moby Dick",
  author = author:alice;  -- Direct reference

SELECT * FROM book WHERE author = author:alice;
```

**Graph Edges** (rich relationships):

```sql
RELATE author:alice->wrote->book:moby
  SET year = 1851, royalties = 0.15;

SELECT ->wrote->book FROM author:alice;
```

**When to use which:**
- **Record links**: Simple references, one-to-many
- **Graph edges**: Many-to-many, rich metadata, complex traversal

### Referential Integrity (v2.2+)

```sql
-- Foreign key-like constraints
DEFINE FIELD author ON book
  TYPE record<author>
  REFERENCE DELETE CASCADE;  -- Delete books when author deleted

CREATE book:test SET
  title = "Test",
  author = author:nonexistent;  -- ❌ Error: author doesn't exist!
```

### Multi-Model Flexibility

**Same database, different models:**

```sql
-- Document model
CREATE user:alice CONTENT {
  name: "Alice",
  email: "alice@example.com",
  settings: {
    theme: "dark",
    notifications: true
  }
};

-- Graph model
RELATE user:alice->posted->content:article123;

-- Relational model (via record links)
CREATE order:ord1 SET
  user = user:alice,
  items = [product:laptop, product:mouse],
  total = 2549.97;

-- Key-value model
CREATE config:app SET data = { ... };
SELECT data FROM config:app;

-- Time-series
CREATE metrics:cpu SET
  value = 78.5,
  timestamp = time::now();
```

**No need to choose paradigm - use all!**

---

## Graph Databases (Neo4j)

[**Neo4j**](https://neo4j.com) is the leading graph database (since 2007).

### Reactive Architecture (v4.0+)

Neo4j 4.0 introduced **reactive architecture**:
- Responsive (low latency)
- Resilient (fault-tolerant)
- Elastic (scales)
- Message-driven (async)

**Enables reactive applications!**

### Change Data Capture (CDC)

Neo4j tracks changes automatically:
- Inserts, updates, deletes
- Stream changes to applications
- Perfect for event-driven systems

### Real-Time Analytics (Infinigraph 2025)

**New feature:** Run OLTP (transactions) + OLAP (analytics) together:
- 100TB+ databases
- Real-time queries + broad pattern analysis
- Fraud detection (hundreds of TB)

### Cypher Query Language

```cypher
// Create nodes
CREATE (alice:Person {name: "Alice", age: 30})
CREATE (bob:Person {name: "Bob", age: 25})
CREATE (post:Post {title: "Hello"})

// Create relationships
CREATE (alice)-[:WROTE {date: "2025-01-20"}]->(post)
CREATE (bob)-[:LIKES]->(post)
CREATE (alice)-[:FRIENDS_WITH]->(bob)

// Query: Alice's posts
MATCH (alice:Person {name: "Alice"})-[:WROTE]->(post:Post)
RETURN post.title

// Query: Who liked Alice's posts?
MATCH (alice:Person {name: "Alice"})-[:WROTE]->(post)<-[:LIKES]-(liker)
RETURN liker.name

// Query: Friends of friends
MATCH (alice:Person {name: "Alice"})-[:FRIENDS_WITH*2]-(fof)
RETURN DISTINCT fof.name
```

**More verbose than SurrealDB, but powerful.**

---

## When to Use Each Model

### Relational (Cell-style)

**Use when:**
- ✅ Complex domain model (many entities, relationships)
- ✅ Integrity constraints critical (keys, foreign keys)
- ✅ Omnidirectional queries needed
- ✅ Schema stability important

**Examples:**
- ERP systems (suppliers, parts, customers, orders)
- Scientific data (atoms, bonds, molecules)
- HR systems (employees, departments, roles)

**Advantages:**
- Declarative integrity
- Flexible queries
- No redundancy
- Type safety (with typed IDs)

**Disadvantages:**
- Requires careful schema design
- Learning curve (different from objects)

### Graph (SurrealDB/Neo4j style)

**Use when:**
- ✅ Relationships are first-class (as important as entities)
- ✅ Deep traversal needed (friends of friends of friends...)
- ✅ Many-to-many relationships common
- ✅ Recommendation engines, social networks

**Examples:**
- Social networks (users, friendships, posts, likes)
- Recommendation systems (user preferences, product similarities)
- Fraud detection (transaction patterns)
- Knowledge graphs (concepts, relationships)

**Advantages:**
- Natural relationship modeling
- Fast graph traversal
- Intuitive queries (arrow syntax)
- Flexible schema

**Disadvantages:**
- Can be overkill for simple data
- Query performance varies with graph size

### Document (MongoDB style)

**Use when:**
- ✅ Schema evolves frequently
- ✅ Nested/hierarchical data
- ✅ Each document independent
- ✅ Flexible structure needed

**Examples:**
- Content management (blog posts, comments)
- Product catalogs (varying attributes)
- User profiles (different fields per user)
- Event logs (varying event types)

**Advantages:**
- Schema flexibility
- Natural JSON mapping
- Easy to scale (sharding)

**Disadvantages:**
- Weak joins (not designed for complex relationships)
- Data duplication common
- Consistency harder

### Key-Value (TABLE style)

**Use when:**
- ✅ Simple lookups by ID/key
- ✅ Caching
- ✅ Session storage
- ✅ High performance critical

**Examples:**
- User sessions (session_id → session_data)
- Cache (cache_key → cached_value)
- Counters (counter_id → count)

**Advantages:**
- Simplest model
- Fastest lookups (O(1))
- Easy to understand

**Disadvantages:**
- No relationships
- Limited querying (only by key)
- Not good for complex data

### Decision Matrix

| Model | Relationships | Query Flexibility | Schema | Performance | Use Case |
|-------|---------------|------------------|--------|-------------|----------|
| **Relational** | Foreign keys | ✅✅✅ (SQL) | Strict | Good | Complex domains |
| **Graph** | ✅✅✅ First-class | ✅✅✅ (Traversal) | Flexible | Variable | Connected data |
| **Document** | Weak | ✅✅ (Queries) | ✅✅✅ Flexible | Good | Hierarchical |
| **Key-Value** | ❌ None | ❌ (Key only) | ✅✅✅ None | ✅✅✅ Fastest | Simple lookups |

---

## Proposal for Boon

### Three Data Models

**Extend Boon with:**

1. **TABLE** (key-value) - Already proposed
2. **RELATION** (Cell-inspired) - New!
3. **GRAPH** (SurrealDB-inspired) - New!

Each serves different needs, all reactive!

---

### 1. TABLE (Key-Value)

**Already designed** (see TABLE_BYTES_RESEARCH.md)

```boon
users: TABLE { UserId, User }
    |> Table/insert(key: user.id, value: user)

user: users |> Table/get(key: user_id)
```

**Use for:** Simple lookups, caching, sessions.

---

### 2. RELATION (Cell-Inspired)

#### **Syntax Proposal**

**Unary (sets):**

```boon
-- Entity existence
suppliers: RELATION { SupplierId }

-- Insert
suppliers |> Relation/insert(supplier_id)

-- Check
exists: suppliers |> Relation/contains(supplier_id)
```

**Binary (key-value with constraints):**

```boon
-- Supplier name (one-to-one)
supplier_names: RELATION { SupplierId, Text }
    |> Relation/key(0)  -- SupplierId unique

-- Insert
supplier_names |> Relation/insert(supplier_id, TEXT { ACME Corp })

-- Query forward
name: supplier_names |> Relation/get(supplier_id)  -- → Text

-- Query reverse (unique on column 1 too)
supplier_names |> Relation/key(1)  -- Both directions unique
id: supplier_names |> Relation/get_reverse(TEXT { ACME Corp })  -- → SupplierId
```

**Ternary (junction with multiple keys):**

```boon
-- Supplier sells part at price
sales: RELATION { SupplierId, PartId, Price }
    |> Relation/key([0, 1])  -- (Supplier, Part) unique

-- Insert
sales |> Relation/insert(supplier_id, part_id, 99.99)

-- Query: What's the price?
price: sales |> Relation/get(supplier_id, part_id)  -- → Price

-- Query: Who sells this part?
suppliers: sales
    |> Relation/where(part: part_id)
    |> Relation/select(supplier)  -- LIST { SupplierId }

-- Query: What does this supplier sell?
parts: sales
    |> Relation/where(supplier: supplier_id)
    |> Relation/select(part)  -- LIST { PartId }
```

**Symmetric (bidirectional):**

```boon
-- Friendship (A friends with B ⟺ B friends with A)
friendships: RELATION { UserId, UserId }
    |> Relation/symmetric()  -- Enforce symmetry

-- Insert (both directions automatically)
friendships |> Relation/insert(alice_id, bob_id)
// Creates: (alice, bob) AND (bob, alice)

-- Query
alice_friends: friendships
    |> Relation/where(user: alice_id)
    |> Relation/select(friend)
```

#### **Foreign Keys**

```boon
-- Books must have authors
written_by: RELATION { BookId, AuthorId }
    |> Relation/foreign_key(
        book: books       -- BookId must exist in books relation
        author: authors   -- AuthorId must exist in authors relation
    )

-- Mandatory authorship
books: RELATION { BookId, Title }
    |> Relation/mandatory(written_by)  -- Every book must have author

-- Insert
books |> Relation/insert(book_id, TEXT { Moby Dick })
written_by |> Relation/insert(book_id, author_id)  -- Must exist!
```

#### **Typed IDs**

```boon
-- Define typed ID types
UserId: TYPE { user_id: Number }
ProductId: TYPE { product_id: Number }

-- Use in relations
users: RELATION { UserId, User }
products: RELATION { ProductId, Product }

-- Compiler prevents mixing
users |> Relation/insert(product_id, user_data)
// ❌ Type error: expected UserId, got ProductId
```

---

### 3. GRAPH (SurrealDB-Inspired)

#### **Syntax Proposal**

**Define nodes:**

```boon
-- Nodes are just TABLEs
users: TABLE { UserId, User }
posts: TABLE { PostId, Post }
products: TABLE { ProductId, Product }
```

**Define edges:**

```boon
-- Edge: user wrote post
wrote: GRAPH {
    from: users
    to: posts
    properties: [timestamp: Time]
}

-- Insert edge
wrote |> Graph/relate(
    from: alice_id
    to: post_id
    properties: [timestamp: now()]
)

-- Edge: user likes post
likes: GRAPH {
    from: users
    to: posts
    properties: [timestamp: Time]
}

likes |> Graph/relate(from: bob_id, to: post_id)

-- Edge: bidirectional (symmetric)
friends_with: GRAPH {
    from: users
    to: users
    symmetric: True  -- Both directions
    properties: [since: Date]
}

friends_with |> Graph/relate(
    from: alice_id
    to: bob_id
    properties: [since: date(TEXT { 2020-01-01 })]
)
```

#### **Graph Traversal**

```boon
-- Forward: Alice's posts
alice_posts: wrote
    |> Graph/from(user: alice_id)
    |> Graph/to_nodes()  -- LIST { Post }

-- Backward: Who wrote this post?
post_author: wrote
    |> Graph/to(post: post_id)
    |> Graph/from_nodes()  -- LIST { User }

-- Multi-hop: Who liked Alice's posts?
post_likers: wrote
    |> Graph/from(user: alice_id)
    |> Graph/traverse(likes)  -- Follow 'likes' edges
    |> Graph/from_nodes()  -- LIST { User }

-- Bidirectional: Alice's friends
alice_friends: friends_with
    |> Graph/node(user: alice_id)
    |> Graph/connected_nodes()  -- LIST { User }

-- Recursive: Friends of friends (2 hops)
friends_of_friends: friends_with
    |> Graph/from(user: alice_id)
    |> Graph/traverse_depth(hops: 2)
    |> Graph/nodes()

-- Shortest path
path: friends_with
    |> Graph/shortest_path(
        from: alice_id
        to: target_id
    )  -- LIST { UserId }
```

#### **Reactive Graph Queries**

```boon
-- Live query: Alice's posts (updates when new posts!)
alice_posts: wrote
    |> Graph/from(user: alice_id)
    |> Graph/to_nodes()
    |> Graph/live()  -- Subscribe to changes!

// When Alice writes new post:
// → wrote edge added
// → alice_posts LIST updates automatically
// → UI re-renders!
```

---

## Complete Examples

### Example 1: E-Commerce (All Three Models)

**Entities as TABLEs:**

```boon
-- Simple lookups
users: TABLE { UserId, User }
products: TABLE { ProductId, Product }
orders: TABLE { OrderId, Order }
```

**Inventory as RELATION:**

```boon
-- Supplier sells product at price
inventory: RELATION { SupplierId, ProductId, Price, Stock }
    |> Relation/key([0, 1])  -- (Supplier, Product) unique

-- Queries
price: inventory
    |> Relation/get(supplier_id, product_id)  -- → Price

suppliers: inventory
    |> Relation/where(product: product_id)
    |> Relation/select(supplier)  -- Who sells this?

products: inventory
    |> Relation/where(supplier: supplier_id)
    |> Relation/select(product)  -- What does supplier sell?
```

**Social features as GRAPH:**

```boon
-- User reviews product
reviews: GRAPH {
    from: users
    to: products
    properties: [rating: Number, text: Text, date: Date]
}

-- User follows user
follows: GRAPH {
    from: users
    to: users
    properties: [since: Date]
}

-- Product recommendations (friends who bought this also bought...)
recommended_products: BLOCK {
    -- Products bought by user
    user_products: orders
        |> Table/get(key: user_id)
        |> Order/products()

    -- Friends of user
    user_friends: follows
        |> Graph/from(user: user_id)
        |> Graph/to_nodes()

    -- Products bought by friends
    friend_products: user_friends
        |> List/flat_map(friend:
            orders
                |> Table/get(key: friend.id)
                |> Order/products()
        )

    -- Exclude already owned
    friend_products
        |> List/filter(product:
            user_products
                |> List/contains(product)
                |> Bool/not()
        )
}
```

**Benefits:**
- TABLE: Fast user/product/order lookups
- RELATION: Flexible inventory queries (by supplier OR product)
- GRAPH: Natural social features (reviews, follows, recommendations)

### Example 2: Social Network (GRAPH-Heavy)

```boon
-- Users
users: TABLE { UserId, User }

-- Posts
posts: TABLE { PostId, Post }

-- Friendships (symmetric)
friends_with: GRAPH {
    from: users
    to: users
    symmetric: True
    properties: [since: Date]
}

-- Authorship
wrote: GRAPH {
    from: users
    to: posts
    properties: [timestamp: Time]
}

-- Likes
likes: GRAPH {
    from: users
    to: posts
    properties: [timestamp: Time]
}

-- Comments (post → comment → post for replies)
comments: GRAPH {
    from: users
    to: posts
    properties: [text: Text, timestamp: Time, parent: PostId]
}

-- Feed: Posts from friends
user_feed: BLOCK {
    user_friends: friends_with
        |> Graph/node(user: current_user_id)
        |> Graph/connected_nodes()

    friend_posts: wrote
        |> Graph/from_list(users: user_friends)
        |> Graph/to_nodes()
        |> List/sort_by(post: post.timestamp, order: Descending)
        |> List/take(count: 50)

    friend_posts
}

-- Who should I follow? (friends of friends)
suggested_follows: BLOCK {
    friends_of_friends: friends_with
        |> Graph/from(user: current_user_id)
        |> Graph/traverse_depth(hops: 2)
        |> Graph/nodes()

    current_friends: friends_with
        |> Graph/node(user: current_user_id)
        |> Graph/connected_nodes()

    -- Exclude self and existing friends
    friends_of_friends
        |> List/filter(user:
            user.id != current_user_id
            |> Bool/and(
                current_friends
                    |> List/contains(user)
                    |> Bool/not()
            )
        )
        |> List/take(count: 10)
}
```

### Example 3: ERP System (RELATION-Heavy)

```boon
-- Typed IDs
SupplierId: TYPE { supplier_id: Number }
PartId: TYPE { part_id: Number }
CustomerId: TYPE { customer_id: Number }
OrderId: TYPE { order_id: Number }

-- Entities
suppliers: RELATION { SupplierId }
parts: RELATION { PartId }
customers: RELATION { CustomerId }

-- Attributes
supplier_name: RELATION { SupplierId, Text } |> Relation/key(0)
supplier_address: RELATION { SupplierId, Text } |> Relation/key(0)
part_name: RELATION { PartId, Text } |> Relation/key(0)
part_description: RELATION { PartId, Text } |> Relation/key(0)

-- Complex relationships
inventory: RELATION { SupplierId, PartId, Price, Stock }
    |> Relation/key([0, 1])  -- (Supplier, Part) unique
    |> Relation/foreign_key(supplier: suppliers, part: parts)

orders: RELATION { OrderId, CustomerId, PartId, Quantity, Date }
    |> Relation/foreign_key(customer: customers, part: parts)

-- Queries

// Find supplier by name (reverse lookup)
acme_id: supplier_name
    |> Relation/get_reverse(TEXT { ACME Corp })

// What parts does ACME sell?
acme_parts: inventory
    |> Relation/where(supplier: acme_id)
    |> Relation/select(part)  -- LIST { PartId }

// Who supplies part X?
part_suppliers: inventory
    |> Relation/where(part: part_x)
    |> Relation/select(supplier)  -- LIST { SupplierId }

// Customer order history
customer_orders: orders
    |> Relation/where(customer: customer_id)
    |> Relation/select([order_id, part, quantity, date])

// Most ordered parts
popular_parts: orders
    |> Relation/group_by(part)
    |> Relation/aggregate(quantity: sum)
    |> Relation/sort_by(quantity, order: Descending)
    |> Relation/take(count: 10)
```

---

## Research Summary

### Cell Language

**URL**: https://www.cell-lang.net

**Key Innovation:** Relations in fully reduced form (atomic facts).

**Core Concepts:**
1. **Relations over objects** - No redundancy, omnidirectional queries
2. **Typed IDs** - `user_id(42)` vs `product_id(42)` are different types
3. **Declarative constraints** - Keys, foreign keys built into schema
4. **Separation of schema and code** - Schema stable, code evolves
5. **Relational automata** - Stateful with mutable relations
6. **Reactive automata** - Signal-based (but experimental)

**Advantages:**
- ✅ Omnidirectional navigation
- ✅ Constant-time lookups (automatic indexing)
- ✅ Natural ternary relations (junction tables)
- ✅ No pointer aliasing (value-based)
- ✅ Declarative integrity

**Disadvantages:**
- ❌ Different paradigm (learning curve)
- ❌ Reactive automata experimental
- ❌ Schema design requires thought

**Relevance for Boon:**
- Relations perfect for complex domains
- Typed IDs prevent confusion
- Separation of schema/code matches Boon philosophy
- Omnidirectional queries powerful

### SurrealDB

**URL**: https://surrealdb.com

**Key Innovation:** Multi-model (document + graph + relational + time-series + key-value + geospatial).

**Core Features:**
1. **Graph edges** - `RELATE person:alice->wrote->post:hello`
2. **Bi-directional traversal** - `<-wrote<-` and `->wrote->`
3. **Live queries** - Real-time subscriptions
4. **Referential integrity** - Foreign key constraints (v2.2+)
5. **Recursive traversal** - Shortest path, unlimited depth
6. **SQL-like syntax** - Familiar, no new query language

**Status 2025:**
- Production-ready (enterprise adoption)
- v2.2: Graph algorithms, foreign keys, performance
- Growing ecosystem

**Advantages:**
- ✅ Multi-model flexibility (use what fits)
- ✅ Graph + document + relational together
- ✅ Live queries (reactive!)
- ✅ Intuitive arrow syntax
- ✅ ACID transactions

**Disadvantages:**
- ❌ Relatively new (compared to Postgres, Neo4j)
- ❌ Still evolving (breaking changes possible)

**Relevance for Boon:**
- Multi-model matches Boon's "universal" philosophy
- Live queries = native reactivity
- Graph edges natural for relationships
- Could be backend for Boon TABLE/RELATION/GRAPH

### Neo4j

**URL**: https://neo4j.com

**Key Innovation:** Mature graph database (since 2007), reactive architecture (v4.0).

**Core Features:**
1. **Reactive architecture** - Responsive, resilient, elastic
2. **Change Data Capture** - Track changes automatically
3. **Real-time analytics** - OLTP + OLAP (Infinigraph 2025)
4. **Cypher query language** - Powerful graph queries
5. **100TB+ scale** - Enterprise-grade

**Advantages:**
- ✅ Mature, battle-tested
- ✅ Reactive architecture
- ✅ Scales massively
- ✅ Strong ecosystem

**Disadvantages:**
- ❌ Graph-only (not multi-model)
- ❌ Cypher more verbose than SurrealDB
- ❌ Expensive (enterprise licensing)

**Relevance for Boon:**
- Reactive architecture inspiration
- CDC for change tracking
- Proof graph databases work at scale

### Comparison: Relational vs Graph vs Document

| Aspect | Relational (SQL) | Graph (Neo4j/SurrealDB) | Document (MongoDB) |
|--------|------------------|-------------------------|---------------------|
| **Structure** | Tables, rows, columns | Nodes, edges, properties | Collections, documents |
| **Relationships** | Foreign keys, joins | ✅✅✅ First-class edges | Embedded or references |
| **Query** | SQL | Traversal (Cypher/arrows) | Queries (aggregation) |
| **Schema** | Strict | Flexible | ✅✅✅ Very flexible |
| **Integrity** | ✅✅✅ Strong | Medium | Weak |
| **Performance** | Good (indexed) | ✅✅✅ Fast traversal | ✅✅✅ Fast reads |
| **Use Case** | OLTP, business apps | Social, fraud, recommendations | CMS, catalogs, logs |

**When to use:**
- **Relational**: Complex business logic, integrity critical
- **Graph**: Connected data, deep traversal, social features
- **Document**: Flexible schema, hierarchical data

**SurrealDB's answer:** Use ALL in one database!

---

## Recommendations for Boon

### Multi-Model Approach

**Don't choose - support all!**

```boon
-- TABLE: Simple key-value
cache: TABLE { Key, Value }

-- RELATION: Complex queries, integrity
inventory: RELATION { SupplierId, PartId, Price }
    |> Relation/key([0, 1])
    |> Relation/foreign_key(supplier: suppliers, part: parts)

-- GRAPH: Social features, traversal
follows: GRAPH {
    from: users
    to: users
    properties: [since: Date]
}
```

**Each serves different needs:**
- TABLE: Fast lookups, caching
- RELATION: Complex domains, omnidirectional queries
- GRAPH: Social networks, recommendations, fraud detection

### Backend Support

**All three compile to same backends:**

**ElectricSQL (Postgres):**
- TABLE → Postgres table (simple schema)
- RELATION → Postgres with indexes on all columns
- GRAPH → Postgres with edge tables + `ltree` or recursive CTEs

**SurrealDB:**
- TABLE → SurrealDB table
- RELATION → SurrealDB table with constraints
- GRAPH → SurrealDB `RELATE` (native!)

**NATS KV:**
- TABLE → NATS KV bucket
- RELATION → Not ideal (use Postgres)
- GRAPH → Not ideal (use SurrealDB)

**LocalStorage:**
- TABLE → localStorage keys
- RELATION → Indexed in memory
- GRAPH → Adjacency list in memory

### Syntax Consistency

**All use similar patterns:**

```boon
-- Insert
table |> Table/insert(key, value)
relation |> Relation/insert(arg1, arg2, arg3)
graph |> Graph/relate(from, to, properties)

-- Query
table |> Table/get(key)
relation |> Relation/get(arg1, arg2)
graph |> Graph/from(node)

-- Filter
table |> Table/values() |> List/filter(...)
relation |> Relation/where(field: value)
graph |> Graph/traverse(...) |> Graph/filter(...)
```

**Pipe-first, compositional!**

### Gradual Adoption

**Start simple, add complexity as needed:**

```boon
// Phase 1: Just TABLE (key-value)
users: TABLE { UserId, User }

// Phase 2: Add RELATION for complex queries
inventory: RELATION { SupplierId, PartId, Price }

// Phase 3: Add GRAPH for social features
follows: GRAPH {
    from: users
    to: users
}
```

**Each level adds capability without breaking previous.**

---

## Open Questions

1. **RELATION syntax**: Too verbose? Simpler alternative?
   - Current: `RELATION { A, B, C } |> Relation/key([0, 1])`
   - Alternative: `RELATION { A, B => C }` (arrow for uniqueness)?

2. **GRAPH ownership**: Who owns edges?
   - Store with `from` node?
   - Store with `to` node?
   - Separate edge storage?

3. **Type inference**: Can compiler infer RELATION keys from usage?
   ```boon
   names: RELATION { UserId, Text }
   // Compiler sees: always queried as names(user_id) → text
   // Auto-add key(0)?
   ```

4. **Hardware support**: Can RELATION/GRAPH work in hardware?
   - RELATION: Fixed-size, compile-time known → possible!
   - GRAPH: More complex (dynamic edges) → software-only?

5. **Migration**: How to migrate TABLE → RELATION → GRAPH?
   - Schema evolution
   - Data migration
   - Backward compatibility

6. **Performance**: Auto-indexing cost?
   - RELATION indexes all columns (like Cell)
   - Memory/speed tradeoff
   - Lazy indexing?

---

## Next Steps

1. **Prototype RELATION** in playground
   - Simple binary relations
   - Omnidirectional queries
   - Compare with TABLE

2. **Design GRAPH API** in detail
   - Traversal patterns
   - Recursive queries
   - Reactivity integration

3. **Backend integration**
   - Test with SurrealDB (native GRAPH!)
   - ElectricSQL for RELATION (Postgres)
   - Performance benchmarks

4. **Documentation**
   - When to use TABLE vs RELATION vs GRAPH
   - Migration patterns
   - Performance characteristics

5. **Examples**
   - Social network (GRAPH-heavy)
   - ERP system (RELATION-heavy)
   - E-commerce (mixed)

---

**Conclusion: Boon can support TABLE + RELATION + GRAPH without choosing paradigms - each has strengths for different domains!** 🚀
