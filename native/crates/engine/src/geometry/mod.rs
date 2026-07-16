//! Geometry subsystem — runtime CSG and the authoring geometry built on it.
//!
//! [`csg_runtime`] is the brush→region→mesh core the engine exists for;
//! [`structures`] the free-standing platform/stair authoring tools; [`geom`]
//! the shared, domain-free math primitives both lean on.

pub mod csg_runtime;
pub mod geom;
pub mod structures;
