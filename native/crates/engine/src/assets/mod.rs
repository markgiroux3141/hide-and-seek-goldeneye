//! Asset loading — bringing external files into engine-side data.
//!
//! [`gltf_load`] pulls static geometry out of glTF/GLB; skinned character
//! loading lives in [`crate::skeletal::gltf_skin`], and texture/BMP embedding
//! in [`crate::render::textures`].

pub mod gltf_load;
pub mod textured_model;
