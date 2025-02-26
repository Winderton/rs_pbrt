// Assumptions:
// 1. objects, data, and materials have the same base name:
//    e.g. OBcornellbox, MEcornellbox and MAcornellbox
//    TODO: Find a better solution (following the Object->data pointer?)
// 2. Right now we search for "Camera" for Transform::look_at(...)
//    TODO: get the render camera/transform from the scene (self.scene.camera)
// 3. Smoothness is valid for the whole mesh
//    TODO: store smoothness per polygon, split mesh into smooth and non-smooth parts
// 4. There might be one IM block with a HDR image, which is used for IBL
//    TODO: check if World actually uses "Environment Lighting"
// 5. Lights with type LA_SUN have a position but point always to the origin
//    TODO: use an empty (compass) object as target

// command line options
use structopt::StructOpt;
// std
use std::collections::HashMap;
use std::convert::TryInto;
use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::mem;
use std::path::Path;
use std::sync::Arc;
// pbrt
use rs_pbrt::core::api::{make_accelerator, make_camera, make_film, make_filter, make_sampler};
use rs_pbrt::core::camera::Camera;
use rs_pbrt::core::film::Film;
use rs_pbrt::core::geometry::{Bounds2f, Bounds2i, Normal3f, Point2f, Point2i, Point3f, Vector3f};
use rs_pbrt::core::integrator::{Integrator, SamplerIntegrator};
use rs_pbrt::core::light::Light;
use rs_pbrt::core::material::Material;
use rs_pbrt::core::medium::MediumInterface;
use rs_pbrt::core::mipmap::ImageWrap;
use rs_pbrt::core::paramset::ParamSet;
use rs_pbrt::core::pbrt::degrees;
use rs_pbrt::core::pbrt::{Float, Spectrum};
use rs_pbrt::core::primitive::{GeometricPrimitive, Primitive};
use rs_pbrt::core::sampler::Sampler;
use rs_pbrt::core::scene::Scene;
use rs_pbrt::core::shape::Shape;
use rs_pbrt::core::texture::{Texture, TextureMapping2D, UVMapping2D};
use rs_pbrt::core::transform::{AnimatedTransform, Transform};
use rs_pbrt::integrators::ao::AOIntegrator;
use rs_pbrt::integrators::bdpt::BDPTIntegrator;
use rs_pbrt::integrators::directlighting::{DirectLightingIntegrator, LightStrategy};
use rs_pbrt::integrators::mlt::MLTIntegrator;
use rs_pbrt::integrators::path::PathIntegrator;
use rs_pbrt::integrators::sppm::SPPMIntegrator;
use rs_pbrt::integrators::volpath::VolPathIntegrator;
use rs_pbrt::integrators::whitted::WhittedIntegrator;
use rs_pbrt::lights::diffuse::DiffuseAreaLight;
use rs_pbrt::lights::distant::DistantLight;
use rs_pbrt::lights::infinite::InfiniteAreaLight;
use rs_pbrt::lights::point::PointLight;
use rs_pbrt::materials::glass::GlassMaterial;
use rs_pbrt::materials::matte::MatteMaterial;
use rs_pbrt::materials::metal::MetalMaterial;
use rs_pbrt::materials::metal::{COPPER_K, COPPER_N, COPPER_SAMPLES, COPPER_WAVELENGTHS};
use rs_pbrt::materials::mirror::MirrorMaterial;
use rs_pbrt::shapes::cylinder::Cylinder;
use rs_pbrt::shapes::disk::Disk;
use rs_pbrt::shapes::sphere::Sphere;
use rs_pbrt::shapes::triangle::{Triangle, TriangleMesh};
use rs_pbrt::textures::constant::ConstantTexture;
use rs_pbrt::textures::imagemap::convert_to_spectrum;
use rs_pbrt::textures::imagemap::ImageTexture;

pub const VERSION: &'static str = env!("CARGO_PKG_VERSION");

/// Parse a Blender scene file and render it.
#[derive(StructOpt)]
struct Cli {
    /// camera name
    #[structopt(short = "c", long = "camera_name")]
    camera_name: Option<String>,
    /// global light scaling
    #[structopt(short = "l", long = "light_scale", default_value = "1.0")]
    light_scale: f32,
    /// pixel samples
    #[structopt(short = "s", long = "samples", default_value = "1")]
    samples: u32,
    /// ao, directlighting, whitted, path, bdpt, mlt, sppm, volpath
    #[structopt(short = "i", long = "integrator")]
    integrator: Option<String>,
    /// max length of a light-carrying path
    #[structopt(short = "m", long = "max_depth", default_value = "5")]
    max_depth: u32,
    /// bootstrap samples [MLT]
    #[structopt(long = "bootstrap_samples", default_value = "100000")]
    bootstrap_samples: u32,
    /// number of Markov chains [MLT]
    #[structopt(long = "chains", default_value = "1000")]
    chains: u32,
    /// number of path mutations [MLT]
    #[structopt(long = "mutations_per_pixel", default_value = "100")]
    mutations_per_pixel: u32,
    /// prob of discarding path [MLT]
    #[structopt(long = "step_probability", default_value = "0.3")]
    step_probability: f32,
    /// perturbation deviation [MLT]
    #[structopt(long = "sigma", default_value = "0.01")]
    sigma: f32,
    /// frequency to write image [SPPM]
    #[structopt(long = "write_frequency", default_value = "1")]
    write_frequency: i32,
    /// The path to the file to read
    #[structopt(parse(from_os_str))]
    path: std::path::PathBuf,
}

// PBRT

#[derive(Debug, Default, Copy, Clone)]
struct PbrtSphere {
    pub radius: f32,
    pub zmin: f32,
    pub zmax: f32,
    pub phimax: f32,
}

impl PbrtSphere {
    fn new(radius: f32, zmin: f32, zmax: f32, phimax: f32) -> Self {
        PbrtSphere {
            radius,
            zmin,
            zmax,
            phimax,
        }
    }
}

#[derive(Debug, Default, Copy, Clone)]
struct PbrtCylinder {
    pub radius: f32,
    pub zmin: f32,
    pub zmax: f32,
    pub phimax: f32,
}

impl PbrtCylinder {
    fn new(radius: f32, zmin: f32, zmax: f32, phimax: f32) -> Self {
        PbrtCylinder {
            radius,
            zmin,
            zmax,
            phimax,
        }
    }
}

#[derive(Debug, Default, Copy, Clone)]
struct PbrtDisk {
    pub height: f32,
    pub radius: f32,
    pub innerradius: f32,
    pub phimax: f32,
}

impl PbrtDisk {
    fn new(height: f32, radius: f32, innerradius: f32, phimax: f32) -> Self {
        PbrtDisk {
            height,
            radius,
            innerradius,
            phimax,
        }
    }
}

// Blender

#[derive(Debug, Default, Copy, Clone)]
struct BlendCamera {
    pub lens: f32,
    pub angle_x: f32,
    pub angle_y: f32,
    pub clipsta: f32,
}

#[derive(Debug, Default, Copy, Clone)]
struct Blend279Material {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
    pub specr: f32,
    pub specg: f32,
    pub specb: f32,
    pub mirr: f32,
    pub mirg: f32,
    pub mirb: f32,
    pub emit: f32,
    pub ang: f32, // IOR
    pub ray_mirror: f32,
    pub roughness: f32,
}

fn focallength_to_fov(focal_length: f32, sensor: f32) -> f32 {
    2.0_f32 * ((sensor / 2.0_f32) / focal_length).atan()
}

// TMP (see pbrt_spheres_differentials_texfilt.rs)

struct SceneDescription {
    // mesh
    mesh_names: Vec<String>,
    meshes: Vec<Arc<TriangleMesh>>,
    triangle_colors: Vec<Vec<Spectrum>>,
    // cylinder
    cylinder_names: Vec<String>,
    cylinders: Vec<Arc<Shape>>,
    // disk
    disk_names: Vec<String>,
    disks: Vec<Arc<Shape>>,
    // sphere
    sphere_names: Vec<String>,
    spheres: Vec<Arc<Shape>>,
    // lights
    lights: Vec<Arc<Light>>,
}

struct SceneDescriptionBuilder {
    // mesh
    mesh_names: Vec<String>,
    meshes: Vec<Arc<TriangleMesh>>,
    triangle_colors: Vec<Vec<Spectrum>>,
    // cylinder
    cylinder_names: Vec<String>,
    cylinders: Vec<Arc<Shape>>,
    // disk
    disk_names: Vec<String>,
    disks: Vec<Arc<Shape>>,
    // sphere
    sphere_names: Vec<String>,
    spheres: Vec<Arc<Shape>>,
    // lights
    lights: Vec<Arc<Light>>,
}

impl SceneDescriptionBuilder {
    fn new() -> SceneDescriptionBuilder {
        SceneDescriptionBuilder {
            mesh_names: Vec::with_capacity(256),
            meshes: Vec::with_capacity(256),
            triangle_colors: Vec::with_capacity(256),
            cylinder_names: Vec::new(),
            cylinders: Vec::new(),
            disk_names: Vec::new(),
            disks: Vec::new(),
            sphere_names: Vec::new(),
            spheres: Vec::new(),
            lights: Vec::new(),
        }
    }
    fn add_mesh(
        &mut self,
        base_name: String,
        object_to_world: Transform,
        world_to_object: Transform,
        n_triangles: u32,
        vertex_indices: Vec<u32>,
        n_vertices: u32,
        p_ws: Vec<Point3f>,
        s: Vec<Vector3f>,
        n_ws: Vec<Normal3f>,
        uv: Vec<Point2f>,
        triangle_colors: Vec<Spectrum>,
    ) -> &mut SceneDescriptionBuilder {
        self.mesh_names.push(base_name);
        let triangle_mesh = Arc::new(TriangleMesh::new(
            object_to_world,
            world_to_object,
            false,
            n_triangles.try_into().unwrap(),
            vertex_indices.try_into().unwrap(),
            n_vertices,
            p_ws, // in world space
            s,    // empty
            n_ws, // in world space
            uv,
            None,
            None,
        ));
        self.meshes.push(triangle_mesh);
        self.triangle_colors.push(triangle_colors);
        self
    }
    fn add_cylinder(
        &mut self,
        base_name: String,
        object_to_world: Transform,
        world_to_object: Transform,
        radius: Float,
        z_min: Float,
        z_max: Float,
        phi_max: Float,
    ) -> &mut SceneDescriptionBuilder {
        self.cylinder_names.push(base_name);
        let cylinder = Arc::new(Shape::Clndr(Cylinder::new(
            object_to_world,
            world_to_object,
            false,
            radius,
            z_min,
            z_max,
            phi_max,
        )));
        self.cylinders.push(cylinder);
        self
    }
    fn add_disk(
        &mut self,
        base_name: String,
        object_to_world: Transform,
        world_to_object: Transform,
        height: Float,
        radius: Float,
        inner_radius: Float,
        phi_max: Float,
    ) -> &mut SceneDescriptionBuilder {
        self.disk_names.push(base_name);
        let disk = Arc::new(Shape::Dsk(Disk::new(
            object_to_world,
            world_to_object,
            false,
            height,
            radius,
            inner_radius,
            phi_max,
        )));
        self.disks.push(disk);
        self
    }
    fn add_sphere(
        &mut self,
        base_name: String,
        object_to_world: Transform,
        world_to_object: Transform,
        radius: Float,
        z_min: Float,
        z_max: Float,
        phi_max: Float,
    ) -> &mut SceneDescriptionBuilder {
        self.sphere_names.push(base_name);
        let sphere = Arc::new(Shape::Sphr(Sphere::new(
            object_to_world,
            world_to_object,
            false,
            radius,
            z_min,
            z_max,
            phi_max,
        )));
        self.spheres.push(sphere);
        self
    }
    fn add_hdr_light(
        &mut self,
        light_to_world: Transform,
        texmap: String,
        light_scale: f32,
    ) -> &mut SceneDescriptionBuilder {
        let l: Spectrum = Spectrum::new(1.0 as Float);
        let sc: Spectrum = Spectrum::new(light_scale as Float);
        let n_samples: i32 = 1;
        let infinte_light = Arc::new(Light::InfiniteArea(Box::new(InfiniteAreaLight::new(
            &light_to_world,
            &(l * sc),
            n_samples,
            texmap,
        ))));
        self.lights.push(infinte_light);
        self
    }
    fn add_distant_light(
        &mut self,
        light_to_world: Transform,
        l: Spectrum,
        light_scale: f32,
    ) -> &mut SceneDescriptionBuilder {
        let sc: Spectrum = Spectrum::new(light_scale as Float);
        let mut from: Point3f = Point3f {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        };
        let to: Point3f = Point3f {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        };
        from = light_to_world.transform_point(&from);
        let dir: Vector3f = from - to;
        let object_to_world: Transform = Transform::new(
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        );
        let distant_light = Arc::new(Light::Distant(Box::new(DistantLight::new(
            &object_to_world,
            &(l * sc),
            &dir,
        ))));
        self.lights.push(distant_light);
        self
    }
    fn add_point_light(
        &mut self,
        light_to_world: Transform,
        l: Spectrum,
        light_scale: f32,
    ) -> &mut SceneDescriptionBuilder {
        let sc: Spectrum = Spectrum::new(light_scale as Float);
        let medium_interface: MediumInterface = MediumInterface::default();
        let point_light = Arc::new(Light::Point(Box::new(PointLight::new(
            &light_to_world,
            &medium_interface,
            &(l * sc),
        ))));
        self.lights.push(point_light);
        self
    }
    fn finalize(self) -> SceneDescription {
        SceneDescription {
            mesh_names: self.mesh_names,
            meshes: self.meshes,
            triangle_colors: self.triangle_colors,
            cylinder_names: self.cylinder_names,
            cylinders: self.cylinders,
            disk_names: self.disk_names,
            disks: self.disks,
            sphere_names: self.sphere_names,
            spheres: self.spheres,
            lights: self.lights,
        }
    }
}

struct RenderOptions {
    has_emitters: bool,
    primitives: Vec<Arc<Primitive>>,
    shapes: Vec<Arc<Shape>>,
    shape_materials: Vec<Arc<Material>>,
    shape_lights: Vec<Option<Arc<Light>>>,
    lights: Vec<Arc<Light>>,
}

impl RenderOptions {
    fn new(
        scene: SceneDescription,
        material_hm: &HashMap<String, Blend279Material>,
        texture_hm: &HashMap<String, OsString>,
        light_scale: Float,
    ) -> RenderOptions {
        let mut has_emitters: bool = false;
        let primitives: Vec<Arc<Primitive>> = Vec::new();
        let mut shapes: Vec<Arc<Shape>> = Vec::new();
        let mut shape_materials: Vec<Arc<Material>> = Vec::new();
        let mut shape_lights: Vec<Option<Arc<Light>>> = Vec::new();
        let mut lights: Vec<Arc<Light>> = Vec::new();
        // default material
        let kd = Arc::new(ConstantTexture::new(Spectrum::new(1.0)));
        let sigma = Arc::new(ConstantTexture::new(0.0 as Float));
        let default_material = Arc::new(Material::Matte(Box::new(MatteMaterial::new(
            kd, sigma, None,
        ))));
        // lights
        for light in &scene.lights {
            lights.push(light.clone());
        }
        // cylinders
        for cylinder_idx in 0..scene.cylinders.len() {
            let cylinder = &scene.cylinders[cylinder_idx];
            let cylinder_name = &scene.cylinder_names[cylinder_idx];
            if let Some(mat) = get_material(cylinder_name, material_hm) {
                // println!("{:?}: {:?}", cylinder_name, mat);
                if mat.emit > 0.0 {
                    has_emitters = true;
                    let mi: MediumInterface = MediumInterface::default();
                    let l_emit: Spectrum = Spectrum::rgb(
                        mat.r * mat.emit * light_scale,
                        mat.g * mat.emit * light_scale,
                        mat.b * mat.emit * light_scale,
                    );
                    let n_samples: i32 = 1;
                    let two_sided: bool = false;
                    let area_light: Arc<Light> =
                        Arc::new(Light::DiffuseArea(Box::new(DiffuseAreaLight::new(
                            &cylinder.get_object_to_world(),
                            &mi,
                            &l_emit,
                            n_samples,
                            cylinder.clone(),
                            two_sided,
                        ))));
                    lights.push(area_light.clone());
                    shapes.push(cylinder.clone());
                    shape_materials.push(default_material.clone());
                    let shape_light: Option<Arc<Light>> = Some(area_light.clone());
                    shape_lights.push(shape_light);
                } else {
                    if mat.ang != 1.0 {
                        // GlassMaterial
                        let kr = Arc::new(ConstantTexture::new(Spectrum::new(1.0)));
                        let kt = Arc::new(ConstantTexture::new(Spectrum::rgb(
                            mat.specr, mat.specg, mat.specb,
                        )));
                        let u_roughness = Arc::new(ConstantTexture::new(0.0 as Float));
                        let v_roughness = Arc::new(ConstantTexture::new(0.0 as Float));
                        let index = Arc::new(ConstantTexture::new(mat.ang as Float));
                        let glass = Arc::new(Material::Glass(Box::new(GlassMaterial {
                            kr: kr,
                            kt: kt,
                            u_roughness: u_roughness,
                            v_roughness: v_roughness,
                            index: index,
                            bump_map: None,
                            remap_roughness: true,
                        })));
                        shapes.push(cylinder.clone());
                        shape_materials.push(glass.clone());
                        shape_lights.push(None);
                    } else if mat.ray_mirror > 0.0 {
                        if mat.roughness > 0.0 {
                            // MetalMaterial
                            let copper_n: Spectrum = Spectrum::from_sampled(
                                &COPPER_WAVELENGTHS,
                                &COPPER_N,
                                COPPER_SAMPLES as i32,
                            );
                            let eta: Arc<dyn Texture<Spectrum> + Send + Sync> =
                                Arc::new(ConstantTexture::new(copper_n));
                            let copper_k: Spectrum = Spectrum::from_sampled(
                                &COPPER_WAVELENGTHS,
                                &COPPER_K,
                                COPPER_SAMPLES as i32,
                            );
                            let k: Arc<dyn Texture<Spectrum> + Send + Sync> =
                                Arc::new(ConstantTexture::new(copper_k));
                            let remap_roughness: bool = true;
                            let metal = Arc::new(Material::Metal(Box::new(MetalMaterial::new(
                                eta,
                                k,
                                Arc::new(ConstantTexture::new(mat.roughness as Float)),
                                None,
                                None,
                                None,
                                remap_roughness,
                            ))));
                            shapes.push(cylinder.clone());
                            shape_materials.push(metal.clone());
                            shape_lights.push(None);
                        } else {
                            // MirrorMaterial
                            let kr = Arc::new(ConstantTexture::new(Spectrum::rgb(
                                mat.mirr * mat.ray_mirror,
                                mat.mirg * mat.ray_mirror,
                                mat.mirb * mat.ray_mirror,
                            )));
                            let mirror =
                                Arc::new(Material::Mirror(Box::new(MirrorMaterial::new(kr, None))));
                            shapes.push(cylinder.clone());
                            shape_materials.push(mirror.clone());
                            shape_lights.push(None);
                        }
                    } else {
                        // MatteMaterial
                        let mut kd: Arc<dyn Texture<Spectrum> + Send + Sync> =
                            Arc::new(ConstantTexture::new(Spectrum::rgb(mat.r, mat.g, mat.b)));
                        if let Some(tex) = texture_hm.get(cylinder_name) {
                            // first try texture with exactly the same name as the mesh
                            let su: Float = 1.0;
                            let sv: Float = 1.0;
                            let du: Float = 0.0;
                            let dv: Float = 0.0;
                            let mapping: Box<TextureMapping2D> =
                                Box::new(TextureMapping2D::UV(UVMapping2D {
                                    su: su,
                                    sv: sv,
                                    du: du,
                                    dv: dv,
                                }));
                            let filename: String = String::from(tex.to_str().unwrap());
                            let do_trilinear: bool = false;
                            let max_aniso: Float = 8.0;
                            let wrap_mode: ImageWrap = ImageWrap::Repeat;
                            let scale: Float = 1.0;
                            let gamma: bool = true;
                            kd = Arc::new(ImageTexture::new(
                                mapping,
                                filename,
                                do_trilinear,
                                max_aniso,
                                wrap_mode,
                                scale,
                                gamma,
                                convert_to_spectrum,
                            ));
                        } else {
                            // then remove trailing digits from mesh name
                            let mut ntd: String = String::new();
                            let mut chars = cylinder_name.chars();
                            let mut digits: String = String::new(); // many digits
                            while let Some(c) = chars.next() {
                                if c.is_digit(10_u32) {
                                    // collect digits
                                    digits.push(c);
                                } else {
                                    // push collected digits (if any)
                                    ntd += &digits;
                                    // and reset
                                    digits = String::new();
                                    // push non-digit
                                    ntd.push(c);
                                }
                            }
                            // try no trailing digits (ntd)
                            if let Some(tex) = texture_hm.get(&ntd) {
                                let su: Float = 1.0;
                                let sv: Float = 1.0;
                                let du: Float = 0.0;
                                let dv: Float = 0.0;
                                let mapping: Box<TextureMapping2D> =
                                    Box::new(TextureMapping2D::UV(UVMapping2D {
                                        su: su,
                                        sv: sv,
                                        du: du,
                                        dv: dv,
                                    }));
                                let filename: String = String::from(tex.to_str().unwrap());
                                let do_trilinear: bool = false;
                                let max_aniso: Float = 8.0;
                                let wrap_mode: ImageWrap = ImageWrap::Repeat;
                                let scale: Float = 1.0;
                                let gamma: bool = true;
                                kd = Arc::new(ImageTexture::new(
                                    mapping,
                                    filename,
                                    do_trilinear,
                                    max_aniso,
                                    wrap_mode,
                                    scale,
                                    gamma,
                                    convert_to_spectrum,
                                ));
                            }
                        }
                        let sigma = Arc::new(ConstantTexture::new(0.0 as Float));
                        let matte = Arc::new(Material::Matte(Box::new(MatteMaterial::new(
                            kd,
                            sigma.clone(),
                            None,
                        ))));
                        shapes.push(cylinder.clone());
                        shape_materials.push(matte.clone());
                        shape_lights.push(None);
                    }
                }
            }
        }
        // disks
        for disk_idx in 0..scene.disks.len() {
            let disk = &scene.disks[disk_idx];
            let disk_name = &scene.disk_names[disk_idx];
            if let Some(mat) = get_material(disk_name, material_hm) {
                // println!("{:?}: {:?}", disk_name, mat);
                if mat.emit > 0.0 {
                    has_emitters = true;
                    let mi: MediumInterface = MediumInterface::default();
                    let l_emit: Spectrum = Spectrum::rgb(
                        mat.r * mat.emit * light_scale,
                        mat.g * mat.emit * light_scale,
                        mat.b * mat.emit * light_scale,
                    );
                    let n_samples: i32 = 1;
                    let two_sided: bool = false;
                    let area_light: Arc<Light> =
                        Arc::new(Light::DiffuseArea(Box::new(DiffuseAreaLight::new(
                            &disk.get_object_to_world(),
                            &mi,
                            &l_emit,
                            n_samples,
                            disk.clone(),
                            two_sided,
                        ))));
                    lights.push(area_light.clone());
                    shapes.push(disk.clone());
                    shape_materials.push(default_material.clone());
                    let shape_light: Option<Arc<Light>> = Some(area_light.clone());
                    shape_lights.push(shape_light);
                } else {
                    if mat.ang != 1.0 {
                        // GlassMaterial
                        let kr = Arc::new(ConstantTexture::new(Spectrum::new(1.0)));
                        let kt = Arc::new(ConstantTexture::new(Spectrum::rgb(
                            mat.specr, mat.specg, mat.specb,
                        )));
                        let u_roughness = Arc::new(ConstantTexture::new(0.0 as Float));
                        let v_roughness = Arc::new(ConstantTexture::new(0.0 as Float));
                        let index = Arc::new(ConstantTexture::new(mat.ang as Float));
                        let glass = Arc::new(Material::Glass(Box::new(GlassMaterial {
                            kr: kr,
                            kt: kt,
                            u_roughness: u_roughness,
                            v_roughness: v_roughness,
                            index: index,
                            bump_map: None,
                            remap_roughness: true,
                        })));
                        shapes.push(disk.clone());
                        shape_materials.push(glass.clone());
                        shape_lights.push(None);
                    } else if mat.ray_mirror > 0.0 {
                        if mat.roughness > 0.0 {
                            // MetalMaterial
                            let copper_n: Spectrum = Spectrum::from_sampled(
                                &COPPER_WAVELENGTHS,
                                &COPPER_N,
                                COPPER_SAMPLES as i32,
                            );
                            let eta: Arc<dyn Texture<Spectrum> + Send + Sync> =
                                Arc::new(ConstantTexture::new(copper_n));
                            let copper_k: Spectrum = Spectrum::from_sampled(
                                &COPPER_WAVELENGTHS,
                                &COPPER_K,
                                COPPER_SAMPLES as i32,
                            );
                            let k: Arc<dyn Texture<Spectrum> + Send + Sync> =
                                Arc::new(ConstantTexture::new(copper_k));
                            let remap_roughness: bool = true;
                            let metal = Arc::new(Material::Metal(Box::new(MetalMaterial::new(
                                eta,
                                k,
                                Arc::new(ConstantTexture::new(mat.roughness as Float)),
                                None,
                                None,
                                None,
                                remap_roughness,
                            ))));
                            shapes.push(disk.clone());
                            shape_materials.push(metal.clone());
                            shape_lights.push(None);
                        } else {
                            // MirrorMaterial
                            let kr = Arc::new(ConstantTexture::new(Spectrum::rgb(
                                mat.mirr * mat.ray_mirror,
                                mat.mirg * mat.ray_mirror,
                                mat.mirb * mat.ray_mirror,
                            )));
                            let mirror =
                                Arc::new(Material::Mirror(Box::new(MirrorMaterial::new(kr, None))));
                            shapes.push(disk.clone());
                            shape_materials.push(mirror.clone());
                            shape_lights.push(None);
                        }
                    } else {
                        // MatteMaterial
                        let mut kd: Arc<dyn Texture<Spectrum> + Send + Sync> =
                            Arc::new(ConstantTexture::new(Spectrum::rgb(mat.r, mat.g, mat.b)));
                        if let Some(tex) = texture_hm.get(disk_name) {
                            // first try texture with exactly the same name as the mesh
                            let su: Float = 1.0;
                            let sv: Float = 1.0;
                            let du: Float = 0.0;
                            let dv: Float = 0.0;
                            let mapping: Box<TextureMapping2D> =
                                Box::new(TextureMapping2D::UV(UVMapping2D {
                                    su: su,
                                    sv: sv,
                                    du: du,
                                    dv: dv,
                                }));
                            let filename: String = String::from(tex.to_str().unwrap());
                            let do_trilinear: bool = false;
                            let max_aniso: Float = 8.0;
                            let wrap_mode: ImageWrap = ImageWrap::Repeat;
                            let scale: Float = 1.0;
                            let gamma: bool = true;
                            kd = Arc::new(ImageTexture::new(
                                mapping,
                                filename,
                                do_trilinear,
                                max_aniso,
                                wrap_mode,
                                scale,
                                gamma,
                                convert_to_spectrum,
                            ));
                        } else {
                            // then remove trailing digits from mesh name
                            let mut ntd: String = String::new();
                            let mut chars = disk_name.chars();
                            let mut digits: String = String::new(); // many digits
                            while let Some(c) = chars.next() {
                                if c.is_digit(10_u32) {
                                    // collect digits
                                    digits.push(c);
                                } else {
                                    // push collected digits (if any)
                                    ntd += &digits;
                                    // and reset
                                    digits = String::new();
                                    // push non-digit
                                    ntd.push(c);
                                }
                            }
                            // try no trailing digits (ntd)
                            if let Some(tex) = texture_hm.get(&ntd) {
                                let su: Float = 1.0;
                                let sv: Float = 1.0;
                                let du: Float = 0.0;
                                let dv: Float = 0.0;
                                let mapping: Box<TextureMapping2D> =
                                    Box::new(TextureMapping2D::UV(UVMapping2D {
                                        su: su,
                                        sv: sv,
                                        du: du,
                                        dv: dv,
                                    }));
                                let filename: String = String::from(tex.to_str().unwrap());
                                let do_trilinear: bool = false;
                                let max_aniso: Float = 8.0;
                                let wrap_mode: ImageWrap = ImageWrap::Repeat;
                                let scale: Float = 1.0;
                                let gamma: bool = true;
                                kd = Arc::new(ImageTexture::new(
                                    mapping,
                                    filename,
                                    do_trilinear,
                                    max_aniso,
                                    wrap_mode,
                                    scale,
                                    gamma,
                                    convert_to_spectrum,
                                ));
                            }
                        }
                        let sigma = Arc::new(ConstantTexture::new(0.0 as Float));
                        let matte = Arc::new(Material::Matte(Box::new(MatteMaterial::new(
                            kd,
                            sigma.clone(),
                            None,
                        ))));
                        shapes.push(disk.clone());
                        shape_materials.push(matte.clone());
                        shape_lights.push(None);
                    }
                }
            }
        }
        // spheres
        for sphere_idx in 0..scene.spheres.len() {
            let sphere = &scene.spheres[sphere_idx];
            let sphere_name = &scene.sphere_names[sphere_idx];
            if let Some(mat) = get_material(sphere_name, material_hm) {
                // println!("{:?}: {:?}", sphere_name, mat);
                if mat.emit > 0.0 {
                    has_emitters = true;
                    let mi: MediumInterface = MediumInterface::default();
                    let l_emit: Spectrum = Spectrum::rgb(
                        mat.r * mat.emit * light_scale,
                        mat.g * mat.emit * light_scale,
                        mat.b * mat.emit * light_scale,
                    );
                    let n_samples: i32 = 1;
                    let two_sided: bool = false;
                    let area_light: Arc<Light> =
                        Arc::new(Light::DiffuseArea(Box::new(DiffuseAreaLight::new(
                            &sphere.get_object_to_world(),
                            &mi,
                            &l_emit,
                            n_samples,
                            sphere.clone(),
                            two_sided,
                        ))));
                    lights.push(area_light.clone());
                    shapes.push(sphere.clone());
                    shape_materials.push(default_material.clone());
                    let shape_light: Option<Arc<Light>> = Some(area_light.clone());
                    shape_lights.push(shape_light);
                } else {
                    if mat.ang != 1.0 {
                        // GlassMaterial
                        let kr = Arc::new(ConstantTexture::new(Spectrum::new(1.0)));
                        let kt = Arc::new(ConstantTexture::new(Spectrum::rgb(
                            mat.specr, mat.specg, mat.specb,
                        )));
                        let u_roughness = Arc::new(ConstantTexture::new(0.0 as Float));
                        let v_roughness = Arc::new(ConstantTexture::new(0.0 as Float));
                        let index = Arc::new(ConstantTexture::new(mat.ang as Float));
                        let glass = Arc::new(Material::Glass(Box::new(GlassMaterial {
                            kr: kr,
                            kt: kt,
                            u_roughness: u_roughness,
                            v_roughness: v_roughness,
                            index: index,
                            bump_map: None,
                            remap_roughness: true,
                        })));
                        shapes.push(sphere.clone());
                        shape_materials.push(glass.clone());
                        shape_lights.push(None);
                    } else if mat.ray_mirror > 0.0 {
                        if mat.roughness > 0.0 {
                            // MetalMaterial
                            let copper_n: Spectrum = Spectrum::from_sampled(
                                &COPPER_WAVELENGTHS,
                                &COPPER_N,
                                COPPER_SAMPLES as i32,
                            );
                            let eta: Arc<dyn Texture<Spectrum> + Send + Sync> =
                                Arc::new(ConstantTexture::new(copper_n));
                            let copper_k: Spectrum = Spectrum::from_sampled(
                                &COPPER_WAVELENGTHS,
                                &COPPER_K,
                                COPPER_SAMPLES as i32,
                            );
                            let k: Arc<dyn Texture<Spectrum> + Send + Sync> =
                                Arc::new(ConstantTexture::new(copper_k));
                            let remap_roughness: bool = true;
                            let metal = Arc::new(Material::Metal(Box::new(MetalMaterial::new(
                                eta,
                                k,
                                Arc::new(ConstantTexture::new(mat.roughness as Float)),
                                None,
                                None,
                                None,
                                remap_roughness,
                            ))));
                            shapes.push(sphere.clone());
                            shape_materials.push(metal.clone());
                            shape_lights.push(None);
                        } else {
                            // MirrorMaterial
                            let kr = Arc::new(ConstantTexture::new(Spectrum::rgb(
                                mat.mirr * mat.ray_mirror,
                                mat.mirg * mat.ray_mirror,
                                mat.mirb * mat.ray_mirror,
                            )));
                            let mirror =
                                Arc::new(Material::Mirror(Box::new(MirrorMaterial::new(kr, None))));
                            shapes.push(sphere.clone());
                            shape_materials.push(mirror.clone());
                            shape_lights.push(None);
                        }
                    } else {
                        // MatteMaterial
                        let mut kd: Arc<dyn Texture<Spectrum> + Send + Sync> =
                            Arc::new(ConstantTexture::new(Spectrum::rgb(mat.r, mat.g, mat.b)));
                        if let Some(tex) = texture_hm.get(sphere_name) {
                            // first try texture with exactly the same name as the mesh
                            let su: Float = 1.0;
                            let sv: Float = 1.0;
                            let du: Float = 0.0;
                            let dv: Float = 0.0;
                            let mapping: Box<TextureMapping2D> =
                                Box::new(TextureMapping2D::UV(UVMapping2D {
                                    su: su,
                                    sv: sv,
                                    du: du,
                                    dv: dv,
                                }));
                            let filename: String = String::from(tex.to_str().unwrap());
                            let do_trilinear: bool = false;
                            let max_aniso: Float = 8.0;
                            let wrap_mode: ImageWrap = ImageWrap::Repeat;
                            let scale: Float = 1.0;
                            let gamma: bool = true;
                            kd = Arc::new(ImageTexture::new(
                                mapping,
                                filename,
                                do_trilinear,
                                max_aniso,
                                wrap_mode,
                                scale,
                                gamma,
                                convert_to_spectrum,
                            ));
                        } else {
                            // then remove trailing digits from mesh name
                            let mut ntd: String = String::new();
                            let mut chars = sphere_name.chars();
                            let mut digits: String = String::new(); // many digits
                            while let Some(c) = chars.next() {
                                if c.is_digit(10_u32) {
                                    // collect digits
                                    digits.push(c);
                                } else {
                                    // push collected digits (if any)
                                    ntd += &digits;
                                    // and reset
                                    digits = String::new();
                                    // push non-digit
                                    ntd.push(c);
                                }
                            }
                            // try no trailing digits (ntd)
                            if let Some(tex) = texture_hm.get(&ntd) {
                                let su: Float = 1.0;
                                let sv: Float = 1.0;
                                let du: Float = 0.0;
                                let dv: Float = 0.0;
                                let mapping: Box<TextureMapping2D> =
                                    Box::new(TextureMapping2D::UV(UVMapping2D {
                                        su: su,
                                        sv: sv,
                                        du: du,
                                        dv: dv,
                                    }));
                                let filename: String = String::from(tex.to_str().unwrap());
                                let do_trilinear: bool = false;
                                let max_aniso: Float = 8.0;
                                let wrap_mode: ImageWrap = ImageWrap::Repeat;
                                let scale: Float = 1.0;
                                let gamma: bool = true;
                                kd = Arc::new(ImageTexture::new(
                                    mapping,
                                    filename,
                                    do_trilinear,
                                    max_aniso,
                                    wrap_mode,
                                    scale,
                                    gamma,
                                    convert_to_spectrum,
                                ));
                            }
                        }
                        let sigma = Arc::new(ConstantTexture::new(0.0 as Float));
                        let matte = Arc::new(Material::Matte(Box::new(MatteMaterial::new(
                            kd,
                            sigma.clone(),
                            None,
                        ))));
                        shapes.push(sphere.clone());
                        shape_materials.push(matte.clone());
                        shape_lights.push(None);
                    }
                }
            }
        }
        // meshes
        for mesh_idx in 0..scene.meshes.len() {
            let mesh = &scene.meshes[mesh_idx];
            let mesh_name = &scene.mesh_names[mesh_idx];
            let triangle_colors = &scene.triangle_colors[mesh_idx];
            // create individual triangles
            let mut triangles: Vec<Arc<Shape>> = Vec::new();
            for id in 0..mesh.n_triangles {
                let triangle = Arc::new(Shape::Trngl(Triangle::new(mesh.clone(), id)));
                triangles.push(triangle.clone());
                shapes.push(triangle.clone());
            }
            if let Some(mat) = get_material(mesh_name, material_hm) {
                // println!("{:?}: {:?}", mesh_name, mat);
                if mat.emit > 0.0 {
                    has_emitters = true;
                    for i in 0..triangles.len() {
                        let triangle = &triangles[i];
                        let mi: MediumInterface = MediumInterface::default();
                        let l_emit: Spectrum = Spectrum::rgb(
                            mat.r * mat.emit * light_scale,
                            mat.g * mat.emit * light_scale,
                            mat.b * mat.emit * light_scale,
                        );
                        let n_samples: i32 = 1;
                        let two_sided: bool = false;
                        let area_light: Arc<Light> =
                            Arc::new(Light::DiffuseArea(Box::new(DiffuseAreaLight::new(
                                &mesh.object_to_world,
                                &mi,
                                &l_emit,
                                n_samples,
                                triangle.clone(),
                                two_sided,
                            ))));
                        lights.push(area_light.clone());
                        shape_materials.push(default_material.clone());
                        let triangle_light: Option<Arc<Light>> = Some(area_light.clone());
                        shape_lights.push(triangle_light);
                    }
                } else {
                    if mat.ang != 1.0 {
                        // GlassMaterial
                        let kr = Arc::new(ConstantTexture::new(Spectrum::new(1.0)));
                        let kt = Arc::new(ConstantTexture::new(Spectrum::rgb(
                            mat.specr, mat.specg, mat.specb,
                        )));
                        let u_roughness = Arc::new(ConstantTexture::new(0.0 as Float));
                        let v_roughness = Arc::new(ConstantTexture::new(0.0 as Float));
                        let index = Arc::new(ConstantTexture::new(mat.ang as Float));
                        let glass = Arc::new(Material::Glass(Box::new(GlassMaterial {
                            kr: kr,
                            kt: kt,
                            u_roughness: u_roughness,
                            v_roughness: v_roughness,
                            index: index,
                            bump_map: None,
                            remap_roughness: true,
                        })));
                        for _i in 0..triangles.len() {
                            shape_materials.push(glass.clone());
                            shape_lights.push(None);
                        }
                    } else if mat.ray_mirror > 0.0 {
                        if mat.roughness > 0.0 {
                            // MetalMaterial
                            let copper_n: Spectrum = Spectrum::from_sampled(
                                &COPPER_WAVELENGTHS,
                                &COPPER_N,
                                COPPER_SAMPLES as i32,
                            );
                            let eta: Arc<dyn Texture<Spectrum> + Send + Sync> =
                                Arc::new(ConstantTexture::new(copper_n));
                            let copper_k: Spectrum = Spectrum::from_sampled(
                                &COPPER_WAVELENGTHS,
                                &COPPER_K,
                                COPPER_SAMPLES as i32,
                            );
                            let k: Arc<dyn Texture<Spectrum> + Send + Sync> =
                                Arc::new(ConstantTexture::new(copper_k));
                            let remap_roughness: bool = true;
                            let metal = Arc::new(Material::Metal(Box::new(MetalMaterial::new(
                                eta,
                                k,
                                Arc::new(ConstantTexture::new(mat.roughness as Float)),
                                None,
                                None,
                                None,
                                remap_roughness,
                            ))));
                            for _i in 0..triangles.len() {
                                shape_materials.push(metal.clone());
                                shape_lights.push(None);
                            }
                        } else {
                            // MirrorMaterial
                            let kr = Arc::new(ConstantTexture::new(Spectrum::rgb(
                                mat.mirr * mat.ray_mirror,
                                mat.mirg * mat.ray_mirror,
                                mat.mirb * mat.ray_mirror,
                            )));
                            let mirror =
                                Arc::new(Material::Mirror(Box::new(MirrorMaterial::new(kr, None))));
                            for _i in 0..triangles.len() {
                                shape_materials.push(mirror.clone());
                                shape_lights.push(None);
                            }
                        }
                    } else {
                        // MatteMaterial
                        let mut kd: Arc<dyn Texture<Spectrum> + Send + Sync> =
                            Arc::new(ConstantTexture::new(Spectrum::rgb(mat.r, mat.g, mat.b)));
                        if let Some(tex) = texture_hm.get(mesh_name) {
                            // first try texture with exactly the same name as the mesh
                            let su: Float = 1.0;
                            let sv: Float = 1.0;
                            let du: Float = 0.0;
                            let dv: Float = 0.0;
                            let mapping: Box<TextureMapping2D> =
                                Box::new(TextureMapping2D::UV(UVMapping2D {
                                    su: su,
                                    sv: sv,
                                    du: du,
                                    dv: dv,
                                }));
                            let filename: String = String::from(tex.to_str().unwrap());
                            let do_trilinear: bool = false;
                            let max_aniso: Float = 8.0;
                            let wrap_mode: ImageWrap = ImageWrap::Repeat;
                            let scale: Float = 1.0;
                            let gamma: bool = true;
                            kd = Arc::new(ImageTexture::new(
                                mapping,
                                filename,
                                do_trilinear,
                                max_aniso,
                                wrap_mode,
                                scale,
                                gamma,
                                convert_to_spectrum,
                            ));
                        } else {
                            // then remove trailing digits from mesh name
                            let mut ntd: String = String::new();
                            let mut chars = mesh_name.chars();
                            let mut digits: String = String::new(); // many digits
                            while let Some(c) = chars.next() {
                                if c.is_digit(10_u32) {
                                    // collect digits
                                    digits.push(c);
                                } else {
                                    // push collected digits (if any)
                                    ntd += &digits;
                                    // and reset
                                    digits = String::new();
                                    // push non-digit
                                    ntd.push(c);
                                }
                            }
                            // try no trailing digits (ntd)
                            if let Some(tex) = texture_hm.get(&ntd) {
                                let su: Float = 1.0;
                                let sv: Float = 1.0;
                                let du: Float = 0.0;
                                let dv: Float = 0.0;
                                let mapping: Box<TextureMapping2D> =
                                    Box::new(TextureMapping2D::UV(UVMapping2D {
                                        su: su,
                                        sv: sv,
                                        du: du,
                                        dv: dv,
                                    }));
                                let filename: String = String::from(tex.to_str().unwrap());
                                let do_trilinear: bool = false;
                                let max_aniso: Float = 8.0;
                                let wrap_mode: ImageWrap = ImageWrap::Repeat;
                                let scale: Float = 1.0;
                                let gamma: bool = true;
                                kd = Arc::new(ImageTexture::new(
                                    mapping,
                                    filename,
                                    do_trilinear,
                                    max_aniso,
                                    wrap_mode,
                                    scale,
                                    gamma,
                                    convert_to_spectrum,
                                ));
                            }
                        }
                        let sigma = Arc::new(ConstantTexture::new(0.0 as Float));
                        let mut matte = Arc::new(Material::Matte(Box::new(MatteMaterial::new(
                            kd,
                            sigma.clone(),
                            None,
                        ))));
                        if triangle_colors.len() != 0_usize {
                            assert!(triangle_colors.len() == triangles.len());
                            // ignore textures, use triangle colors
                            for i in 0..triangles.len() {
                                // overwrite kd
                                kd = Arc::new(ConstantTexture::new(triangle_colors[i]));
                                matte = Arc::new(Material::Matte(Box::new(MatteMaterial::new(
                                    kd,
                                    sigma.clone(),
                                    None,
                                ))));
                                shape_materials.push(matte.clone());
                                shape_lights.push(None);
                            }
                        } else {
                            for _i in 0..triangles.len() {
                                shape_materials.push(matte.clone());
                                shape_lights.push(None);
                            }
                        }
                    }
                }
            } else {
                println!("{:?}: no mat", mesh_name);
                for _i in 0..triangles.len() {
                    shape_materials.push(default_material.clone());
                    shape_lights.push(None);
                }
            }
        }
        RenderOptions {
            has_emitters: has_emitters,
            primitives: primitives,
            shapes: shapes,
            shape_materials: shape_materials,
            shape_lights: shape_lights,
            lights: lights,
        }
    }
}

// TMP

fn decode_blender_header(header: &[u8], version: &mut u32, print_it: bool) -> bool {
    // BLENDER
    match header[0] as char {
        'B' => {
            if print_it {
                print!("B");
            }
        }
        _ => return false,
    }
    match header[1] as char {
        'L' => {
            if print_it {
                print!("L");
            }
        }
        _ => return false,
    }
    match header[2] as char {
        'E' => {
            if print_it {
                print!("E");
            }
        }
        _ => return false,
    }
    match header[3] as char {
        'N' => {
            if print_it {
                print!("N");
            }
        }
        _ => return false,
    }
    match header[4] as char {
        'D' => {
            if print_it {
                print!("D");
            }
        }
        _ => return false,
    }
    match header[5] as char {
        'E' => {
            if print_it {
                print!("E");
            }
        }
        _ => return false,
    }
    match header[6] as char {
        'R' => {
            if print_it {
                print!("R");
            }
        }
        _ => return false,
    }
    // [_|-]
    match header[7] as char {
        '_' => {
            if print_it {
                print!("_");
            }
        }
        '-' => {
            if print_it {
                print!("-");
            }
        }
        _ => return false,
    }
    // [v|V]
    match header[8] as char {
        'v' => {
            if print_it {
                print!("v");
            }
        }
        'V' => {
            if print_it {
                print!("V");
            }
        }
        _ => return false,
    }
    for i in 9..12 {
        if header[i].is_ascii_digit() {
            if print_it {
                print!("{:?}", (header[i] as char).to_digit(10).unwrap());
            }
        } else {
            return false;
        }
    }
    if print_it {
        print!("\n");
    }
    // get the version number
    let last3c = vec![header[9], header[10], header[11]];
    let version_str = String::from_utf8(last3c).unwrap();
    // convert to u32 and return
    *version = version_str.parse::<u32>().unwrap();
    true
}

fn get_material<'s, 'h>(
    mesh_name: &'s String,
    material_hm: &'h HashMap<String, Blend279Material>,
) -> Option<&'h Blend279Material> {
    // first try material with exactly the same name as the mesh
    if let Some(mat) = material_hm.get(mesh_name) {
        return Some(mat);
    } else {
        // then remove trailing digits from mesh name
        let mut ntd: String = String::new();
        let mut chars = mesh_name.chars();
        let mut digits: String = String::new(); // many digits
        while let Some(c) = chars.next() {
            if c.is_digit(10_u32) {
                // collect digits
                digits.push(c);
            } else {
                // push collected digits (if any)
                ntd += &digits;
                // and reset
                digits = String::new();
                // push non-digit
                ntd.push(c);
            }
        }
        // try no trailing digits (ntd)
        if let Some(mat) = material_hm.get(&ntd) {
            return Some(mat);
        } else {
            // finally try adding a '1' at the end
            ntd.push('1');
            if let Some(mat) = material_hm.get(&ntd) {
                return Some(mat);
            } else {
                println!("WARNING: No material found for {:?}", mesh_name);
                return None;
            }
        }
    }
    // material_hm.get(mesh_name)
}

fn make_id(code: &[u8]) -> String {
    let mut id = String::with_capacity(4);
    for i in 0..4 {
        if (code[i] as char).is_ascii_alphanumeric() {
            id.push(code[i] as char);
        }
    }
    id
}

fn read_mesh(
    base_name: &String,
    object_to_world_hm: &HashMap<String, Transform>,
    object_to_world: &mut Transform,
    p: &Vec<Point3f>,
    n: &Vec<Normal3f>,
    uvs: &mut Vec<Point2f>,
    loops: &Vec<u8>,
    vertex_indices: Vec<u32>,
    vertex_colors: Vec<u8>,
    is_smooth: bool,
    builder: &mut SceneDescriptionBuilder,
) {
    let mut instances: Vec<Transform> = Vec::with_capacity(u8::MAX as usize);
    let mut triangle_colors: Vec<Spectrum> = Vec::with_capacity(loops.len() * 2);
    if vertex_colors.len() != 0_usize {
        // let n_vertex_colors: usize = vertex_colors.len() / 4;
        // println!("{:?}: {} vertex colors found", base_name, n_vertex_colors);
        // println!("{:?}: {} loops found", base_name, loops.len());
        // println!("{:?}: {} vertices", base_name, vertex_indices.len());
        let mut rgba: usize = 0;
        for l in loops {
            for i in 0..*l {
                let r: Float = vertex_colors[rgba * 4 + 0] as Float / 255.0 as Float;
                let g: Float = vertex_colors[rgba * 4 + 1] as Float / 255.0 as Float;
                let b: Float = vertex_colors[rgba * 4 + 2] as Float / 255.0 as Float;
                // let a: Float = vertex_colors[rgba*4 + 3] as Float / 255.0 as Float;
                let s: Spectrum = Spectrum::rgb(r, g, b);
                // println!("{}: {:?}", rgba, s);
                if i == 0 {
                    // only store first color in loop (triangle or quad)
                    triangle_colors.push(s);
                    if *l == 4 {
                        // store it twice for quads (two triangles)
                        triangle_colors.push(s);
                    }
                }
                rgba += 1;
            }
        }
    }
    if let Some(o2w) = object_to_world_hm.get(base_name) {
        // one instance
        instances.push(*o2w);
    } else {
        // potentially many instances
        let mut counter: u8 = 0;
        let mut search_name: String;
        loop {
            counter += 1;
            search_name = base_name.to_string() + &counter.to_string();
            if let Some(o2w) = object_to_world_hm.get(&search_name) {
                instances.push(*o2w);
            } else {
                break;
            }
        }
    }
    for o2w in instances {
        *object_to_world = o2w;
        let world_to_object: Transform = Transform::inverse(&object_to_world);
        let n_triangles: usize = vertex_indices.len() / 3;
        // transform mesh vertices to world space
        let mut p_ws: Vec<Point3f> = Vec::with_capacity(p.len());
        let mut n_vertices: usize = p.len();
        for i in 0..n_vertices {
            p_ws.push(object_to_world.transform_point(&p[i]));
        }
        let mut n_ws: Vec<Normal3f> = Vec::with_capacity(n.len());
        if is_smooth {
            // println!("  is_smooth = {}", is_smooth);
            assert!(n.len() == p.len());
            if !n.is_empty() {
                for i in 0..n_vertices {
                    n_ws.push(object_to_world.transform_normal(&n[i]));
                }
            }
        }
        let s: Vec<Vector3f> = Vec::new();
        let mut uv: Vec<Point2f> = Vec::with_capacity(uvs.len());
        let mut new_vertex_indices: Vec<u32> = Vec::with_capacity(vertex_indices.len());
        if !uvs.is_empty() {
            let mut p_ws_vi: Vec<Point3f> = Vec::with_capacity(vertex_indices.len());
            let mut vertex_counter: u32 = 0;
            for vi in &vertex_indices {
                p_ws_vi.push(p_ws[*vi as usize]);
                new_vertex_indices.push(vertex_counter);
                vertex_counter += 1;
            }
            if is_smooth {
                let mut n_ws_vi: Vec<Normal3f> = Vec::with_capacity(vertex_indices.len());
                for vi in &vertex_indices {
                    n_ws_vi.push(n_ws[*vi as usize]);
                }
                n_ws = n_ws_vi;
            }
            let mut new_uvs: Vec<Point2f> = Vec::with_capacity(uvs.len());
            let mut loop_idx: usize = 0;
            for poly in loops {
                // triangle
                if *poly == 3_u8 {
                    for _i in 0..3 {
                        new_uvs.push(uvs[loop_idx]);
                        loop_idx += 1;
                    }
                }
                // quad
                else if *poly == 4_u8 {
                    new_uvs.push(uvs[loop_idx + 0]);
                    new_uvs.push(uvs[loop_idx + 1]);
                    new_uvs.push(uvs[loop_idx + 2]);
                    new_uvs.push(uvs[loop_idx + 0]);
                    new_uvs.push(uvs[loop_idx + 2]);
                    new_uvs.push(uvs[loop_idx + 3]);
                    loop_idx += 4;
                } else {
                    println!("WARNING: quads or triangles expected (poly = {})", poly)
                }
            }
            *uvs = new_uvs;
            assert!(
                uvs.len() == p_ws_vi.len(),
                "{} != {}",
                uvs.len(),
                p_ws_vi.len()
            );
            p_ws = p_ws_vi;
            n_vertices = p_ws.len();
            for i in 0..n_vertices {
                uv.push(uvs[i]);
            }
        }
        if new_vertex_indices.len() != 0 {
            builder.add_mesh(
                base_name.clone(),
                *object_to_world,
                world_to_object,
                n_triangles.try_into().unwrap(),
                new_vertex_indices.clone(),
                n_vertices.try_into().unwrap(),
                p_ws, // in world space
                s,    // empty
                n_ws, // in world space
                uv,
                triangle_colors.clone(),
            );
        } else {
            builder.add_mesh(
                base_name.clone(),
                *object_to_world,
                world_to_object,
                n_triangles.try_into().unwrap(),
                vertex_indices.clone(),
                n_vertices.try_into().unwrap(),
                p_ws, // in world space
                s,    // empty
                n_ws, // in world space
                uv,
                triangle_colors.clone(),
            );
        }
    }
}

fn read_names(
    f: &mut File,
    nr_names: usize,
    names: &mut Vec<String>,
    byte_counter: &mut usize,
) -> std::io::Result<()> {
    // let mut name_counter: usize = 0;
    let mut buffer = [0; 1];
    loop {
        if names.len() == nr_names {
            break;
        } else {
            let mut name = String::new();
            loop {
                // read only one char/byte
                f.read(&mut buffer)?;
                *byte_counter += 1;
                if buffer[0] == 0 {
                    break;
                } else {
                    name.push(buffer[0] as char);
                }
            }
            // println!("  {:?}", name);
            names.push(name);
            // name_counter += 1;
        }
    }
    // println!("  {} names found in {} bytes", name_counter, byte_counter);
    Ok(())
}

fn read_type_names(
    f: &mut File,
    nr_types: usize,
    type_names: &mut Vec<String>,
    byte_counter: &mut usize,
) -> std::io::Result<()> {
    // let mut name_counter: usize = 0;
    let mut buffer = [0; 1];
    loop {
        if type_names.len() == nr_types {
            break;
        } else {
            let mut name = String::new();
            loop {
                // read only one char/byte
                f.read(&mut buffer)?;
                *byte_counter += 1;
                if buffer[0] == 0 {
                    break;
                } else {
                    name.push(buffer[0] as char);
                }
            }
            // println!("  {:?}", name);
            type_names.push(name);
            // name_counter += 1;
        }
    }
    // println!(
    //     "  {} type names found in {} bytes",
    //     name_counter, byte_counter
    // );
    Ok(())
}

pub fn make_perspective_camera(
    filter_width: Float,
    xres: i32,
    yres: i32,
    fov: Float,
    animated_cam_to_world: AnimatedTransform,
    clipsta: Float,
) -> Option<Arc<Camera>> {
    let mut some_camera: Option<Arc<Camera>> = None;
    let mut filter_params: ParamSet = ParamSet::default();
    filter_params.add_float(String::from("xwidth"), filter_width);
    filter_params.add_float(String::from("ywidth"), filter_width);
    let some_filter = make_filter(&String::from("gaussian"), &filter_params);
    if let Some(filter) = some_filter {
        let film_name: String = String::from("image");
        let mut film_params: ParamSet = ParamSet::default();
        film_params.add_int(String::from("xresolution"), xres);
        film_params.add_int(String::from("yresolution"), yres);
        let some_film: Option<Arc<Film>> = make_film(&film_name, &film_params, filter);
        if let Some(film) = some_film {
            let camera_name: String = String::from("perspective");
            let mut camera_params: ParamSet = ParamSet::default();
            camera_params.add_float(String::from("fov"), fov);
            some_camera = make_camera(
                &camera_name,
                &camera_params,
                animated_cam_to_world,
                film,
                clipsta,
            );
        }
    }
    some_camera
}

fn make_integrator(
    integrator_name: &String,
    filter_width: Float,
    xres: i32,
    yres: i32,
    fov: Float,
    clipsta: Float,
    animated_cam_to_world: AnimatedTransform,
    pixelsamples: i32,
    integrator_params: ParamSet,
) -> Option<Box<Integrator>> {
    let mut some_integrator: Option<Box<Integrator>> = None;
    let some_camera: Option<Arc<Camera>> = make_perspective_camera(
        filter_width,
        xres,
        yres,
        fov,
        animated_cam_to_world,
        clipsta,
    );
    if let Some(camera) = some_camera {
        let sampler_name: String = String::from("halton");
        let mut sampler_params: ParamSet = ParamSet::default();
        sampler_params.add_int(String::from("pixelsamples"), pixelsamples);
        let some_sampler: Option<Box<Sampler>> =
            make_sampler(&sampler_name, &sampler_params, camera.get_film());
        if let Some(sampler) = some_sampler {
            print!("integrator = {:?} [", integrator_name);
            if integrator_name == "whitted" {
                println!("Whitted’s Ray-Tracing]");
                println!("  pixelsamples = {}", pixelsamples);
                // CreateWhittedIntegrator
                let max_depth: i32 = integrator_params.find_one_int("maxdepth", 5);
                let pixel_bounds: Bounds2i = camera.get_film().get_sample_bounds();
                let integrator = Box::new(Integrator::Sampler(SamplerIntegrator::Whitted(
                    WhittedIntegrator::new(max_depth as u32, camera, sampler, pixel_bounds),
                )));
                some_integrator = Some(integrator);
            } else if integrator_name == "directlighting" {
                println!("Direct Lighting]");
                println!("  pixelsamples = {}", pixelsamples);
                // CreateDirectLightingIntegrator
                let max_depth: i32 = integrator_params.find_one_int("maxdepth", 5);
                println!("  max_depth = {}", max_depth);
                let st: String = integrator_params.find_one_string("strategy", String::from("all"));
                let strategy: LightStrategy;
                if st == "one" {
                    strategy = LightStrategy::UniformSampleOne;
                } else if st == "all" {
                    strategy = LightStrategy::UniformSampleAll;
                } else {
                    panic!("Strategy \"{}\" for direct lighting unknown.", st);
                }
                let pixel_bounds: Bounds2i = Bounds2i {
                    p_min: Point2i { x: 0, y: 0 },
                    p_max: Point2i { x: xres, y: yres },
                };
                let integrator = Box::new(Integrator::Sampler(SamplerIntegrator::DirectLighting(
                    DirectLightingIntegrator::new(
                        strategy,
                        max_depth as u32,
                        camera,
                        sampler,
                        pixel_bounds,
                    ),
                )));
                some_integrator = Some(integrator);
            } else if integrator_name == "path" {
                println!("(Unidirectional) Path Tracing]");
                println!("  pixelsamples = {}", pixelsamples);
                // CreatePathIntegrator
                let max_depth: i32 = integrator_params.find_one_int("maxdepth", 5);
                println!("  max_depth = {}", max_depth);
                let pixel_bounds: Bounds2i = camera.get_film().get_sample_bounds();
                let rr_threshold: Float = 1.0;
                let light_strategy: String = String::from("spatial");
                let integrator = Box::new(Integrator::Sampler(SamplerIntegrator::Path(
                    PathIntegrator::new(
                        max_depth as u32,
                        camera,
                        sampler,
                        pixel_bounds,
                        rr_threshold,
                        light_strategy,
                    ),
                )));
                some_integrator = Some(integrator);
            } else if integrator_name == "volpath" {
                println!("Path Tracing (Participating Media)]");
                println!("  pixelsamples = {}", pixelsamples);
                // CreateVolPathIntegrator
                let max_depth: i32 = integrator_params.find_one_int("maxdepth", 5);
                let pixel_bounds: Bounds2i = camera.get_film().get_sample_bounds();
                let rr_threshold: Float = 1.0;
                let light_strategy: String = String::from("spatial");
                let integrator = Box::new(Integrator::Sampler(SamplerIntegrator::VolPath(
                    VolPathIntegrator::new(
                        max_depth as u32,
                        camera,
                        sampler,
                        pixel_bounds,
                        rr_threshold,
                        light_strategy,
                    ),
                )));
                some_integrator = Some(integrator);
            } else if integrator_name == "bdpt" {
                println!("Bidirectional Path Tracing (BDPT)]");
                println!("  pixelsamples = {}", pixelsamples);
                // CreateBDPTIntegrator
                let max_depth: i32 = integrator_params.find_one_int("maxdepth", 5);
                println!("  max_depth = {}", max_depth);
                let pixel_bounds: Bounds2i = camera.get_film().get_sample_bounds();
                let light_strategy: String = String::from("power");
                let integrator = Box::new(Integrator::BDPT(BDPTIntegrator::new(
                    camera,
                    sampler,
                    pixel_bounds,
                    max_depth as u32,
                    light_strategy,
                )));
                some_integrator = Some(integrator);
            } else if integrator_name == "mlt" {
                println!("Metropolis Light Transport (MLT)]");
                println!("  pixelsamples = {}", pixelsamples);
                // CreateMLTIntegrator
                let max_depth: i32 = integrator_params.find_one_int("maxdepth", 5);
                println!("  max_depth = {}", max_depth);
                let n_bootstrap: i32 = integrator_params.find_one_int("bootstrapsamples", 100000);
                println!("  bootstrap_samples = {}", n_bootstrap);
                let n_chains: i32 = integrator_params.find_one_int("chains", 1000);
                println!("  chains = {}", n_chains);
                let mutations_per_pixel: i32 =
                    integrator_params.find_one_int("mutationsperpixel", 100);
                println!("  mutations_per_pixel = {}", mutations_per_pixel);
                let large_step_probability: Float =
                    integrator_params.find_one_float("largestepprobability", 0.3 as Float);
                println!("  step_probability = {}", large_step_probability);
                let sigma: Float = integrator_params.find_one_float("sigma", 0.01 as Float);
                println!("  sigma = {}", sigma);
                let integrator = Box::new(Integrator::MLT(MLTIntegrator::new(
                    camera.clone(),
                    max_depth as u32,
                    n_bootstrap as u32,
                    n_chains as u32,
                    mutations_per_pixel as u32,
                    sigma,
                    large_step_probability,
                )));
                some_integrator = Some(integrator);
            } else if integrator_name == "ao" || integrator_name == "ambientocclusion" {
                println!("Ambient Occlusion (AO)]");
                println!("  pixelsamples = {}", pixelsamples);
                // CreateAOIntegrator
                let pixel_bounds: Bounds2i = camera.get_film().get_sample_bounds();
                let cos_sample: bool = integrator_params.find_one_bool("cossample", true);
                let n_samples: i32 = integrator_params.find_one_int("nsamples", 64 as i32);
                let integrator = Box::new(Integrator::Sampler(SamplerIntegrator::AO(
                    AOIntegrator::new(cos_sample, n_samples, camera, sampler, pixel_bounds),
                )));
                some_integrator = Some(integrator);
            } else if integrator_name == "sppm" {
                println!("Stochastic Progressive Photon Mapping (SPPM)]");
                println!("  pixelsamples = {}", pixelsamples);
                // CreateSPPMIntegrator
                let mut n_iterations: i32 = integrator_params.find_one_int("numiterations", 64);
                n_iterations = integrator_params.find_one_int("iterations", n_iterations);
                let max_depth: i32 = integrator_params.find_one_int("maxdepth", 5);
                let photons_per_iter: i32 =
                    integrator_params.find_one_int("photonsperiteration", -1);
                let write_freq: i32 =
                    integrator_params.find_one_int("imagewritefrequency", 1 << 31);
                println!("  imagewritefrequency = {}", write_freq);
                let radius: Float = integrator_params.find_one_float("radius", 1.0 as Float);
                // TODO: if (PbrtOptions.quickRender) nIterations = std::max(1, nIterations / 16);
                let integrator = Box::new(Integrator::SPPM(SPPMIntegrator::new(
                    camera.clone(),
                    n_iterations,
                    photons_per_iter,
                    max_depth as u32,
                    radius,
                    write_freq,
                )));
                some_integrator = Some(integrator);
            } else {
                println!("Integrator \"{}\" unknown.", integrator_name);
            }
        } else {
            panic!("Unable to create sampler.");
        }
    } else {
        panic!("Unable to create camera.");
    }
    some_integrator
}

fn make_scene(primitives: &Vec<Arc<Primitive>>, lights: Vec<Arc<Light>>) -> Scene {
    let accelerator_name: String = String::from("bvh");
    let some_accelerator = make_accelerator(&accelerator_name, &primitives, &ParamSet::default());
    if let Some(accelerator) = some_accelerator {
        return Scene::new(accelerator, lights);
    } else {
        panic!("Unable to create accelerator.");
    }
}

fn main() -> std::io::Result<()> {
    let args = Cli::from_args();
    let git_describe = option_env!("GIT_DESCRIBE").unwrap_or("unknown");
    let num_threads: u8 = num_cpus::get() as u8;
    println!(
        "parse_blend_file version {} ({}) [Detected {} cores]",
        VERSION, git_describe, num_threads
    );
    // PBRT
    let mut scale_length: f32 = 1.0;
    let mut resolution_x: u32 = 640;
    let mut resolution_y: u32 = 480;
    let mut resolution_percentage: u16 = 100;
    let mut angle_x: f32 = 45.0;
    let mut angle_y: f32 = 45.0;
    let mut base_name = String::new();
    let mut camera_hm: HashMap<String, BlendCamera> = HashMap::new();
    let mut texture_hm: HashMap<String, OsString> = HashMap::new();
    let mut spheres_hm: HashMap<String, PbrtSphere> = HashMap::new();
    let mut cylinders_hm: HashMap<String, PbrtCylinder> = HashMap::new();
    let mut disks_hm: HashMap<String, PbrtDisk> = HashMap::new();
    let mut ob_count: usize = 0;
    let mut object_to_world: Transform = Transform::new(
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    );
    let mut p: Vec<Point3f> = Vec::with_capacity(1048576);
    let mut n: Vec<Normal3f> = Vec::with_capacity(1048576);
    let mut uvs: Vec<Point2f> = Vec::with_capacity(1048576);
    let mut loops: Vec<u8> = Vec::with_capacity(1048576);
    let mut vertex_indices: Vec<u32> = Vec::with_capacity(1048576);
    let mut vertex_colors: Vec<u8> = Vec::with_capacity(1048576);
    let mut prop_height: f64 = 0.0;
    let mut prop_radius: f64 = 1.0;
    let mut prop_innerradius: f64 = 0.0;
    let mut prop_zmin: f64 = -1.0;
    let mut prop_zmax: f64 = 1.0;
    let mut prop_phimax: f64 = 360.0;
    let mut hdr_path: OsString = OsString::new();
    // first get the DNA
    let mut names: Vec<String> = Vec::new();
    let mut names_len: usize = 0;
    let mut types: Vec<String> = Vec::new();
    let mut dna_2_type_id: Vec<u16> = Vec::new();
    let mut types_len: usize = 0;
    let mut tlen: Vec<u16> = Vec::new();
    {
        let mut f = File::open(&args.path)?;
        // read exactly 12 bytes
        // let mut counter: usize = 0;
        let mut buffer = [0; 12];
        f.read(&mut buffer)?;
        // counter += 12;
        let mut blender_version: u32 = 0;
        if !decode_blender_header(&buffer, &mut blender_version, true) {
            println!("ERROR: Not a .blend file");
            println!("First 12 bytes:");
            println!("{:?}", buffer);
        } else {
            loop {
                // code
                let mut buffer = [0; 4];
                f.read(&mut buffer)?;
                // counter += 4;
                let code = make_id(&buffer);
                // len
                let mut buffer = [0; 4];
                f.read(&mut buffer)?;
                // counter += 4;
                let mut len: u32 = 0;
                len += (buffer[0] as u32) << 0;
                len += (buffer[1] as u32) << 8;
                len += (buffer[2] as u32) << 16;
                len += (buffer[3] as u32) << 24;
                // for now ignore the old entry
                let mut buffer = [0; 8];
                f.read(&mut buffer)?;
                // counter += 8;
                // get SDNAnr
                let mut buffer = [0; 4];
                f.read(&mut buffer)?;
                // counter += 4;
                // for now ignore the nr entry
                let mut buffer = [0; 4];
                f.read(&mut buffer)?;
                // counter += 4;
                // are we done?
                if code == String::from("ENDB") {
                    break;
                }
                if code == String::from("DNA1") {
                    // println!("{} ({})", code, len);
                    // "SDNANAME" in first 8 bytes
                    let mut buffer = [0; 8];
                    f.read(&mut buffer)?;
                    // counter += 8;
                    let mut sdna_name = String::with_capacity(8);
                    for i in 0..8 {
                        if (buffer[i] as char).is_ascii_alphabetic() {
                            sdna_name.push(buffer[i] as char);
                        }
                    }
                    if sdna_name != String::from("SDNANAME") {
                        // read remaining bytes
                        let mut buffer = vec![0; (len - 8) as usize];
                        f.read(&mut buffer)?;
                    // counter += (len - 8) as usize;
                    } else {
                        let mut buffer = [0; 4];
                        f.read(&mut buffer)?;
                        // counter += 4;
                        let mut nr_names: u32 = 0;
                        nr_names += (buffer[0] as u32) << 0;
                        nr_names += (buffer[1] as u32) << 8;
                        nr_names += (buffer[2] as u32) << 16;
                        nr_names += (buffer[3] as u32) << 24;
                        read_names(&mut f, nr_names as usize, &mut names, &mut names_len)?;
                        // counter += names_len;
                        let mut remaining_bytes: usize = (len - 12) as usize - names_len;
                        // skip pad bytes, read "TYPE" and nr_types
                        let mut buffer = [0; 1];
                        loop {
                            f.read(&mut buffer)?;
                            // counter += 1;
                            if buffer[0] == 0 {
                                // skip pad byte
                                remaining_bytes -= 1;
                            } else if buffer[0] as char == 'T' {
                                remaining_bytes -= 1;
                                break;
                            }
                        }
                        // match 'YPE' ('T' was matched above)
                        let mut buffer = [0; 3];
                        f.read(&mut buffer)?;
                        // counter += 3;
                        remaining_bytes -= 3;
                        if buffer[0] as char == 'Y'
                            && buffer[1] as char == 'P'
                            && buffer[2] as char == 'E'
                        {
                            // nr_types
                            let mut buffer = [0; 4];
                            f.read(&mut buffer)?;
                            // counter += 4;
                            remaining_bytes -= 4;
                            let mut nr_types: u32 = 0;
                            nr_types += (buffer[0] as u32) << 0;
                            nr_types += (buffer[1] as u32) << 8;
                            nr_types += (buffer[2] as u32) << 16;
                            nr_types += (buffer[3] as u32) << 24;
                            read_type_names(&mut f, nr_types as usize, &mut types, &mut types_len)?;
                            // counter += types_len;
                            remaining_bytes -= types_len;
                            // skip pad bytes, read "TLEN"
                            let mut buffer = [0; 1];
                            loop {
                                f.read(&mut buffer)?;
                                // counter += 1;
                                if buffer[0] == 0 {
                                    // skip pad byte
                                    remaining_bytes -= 1;
                                } else if buffer[0] as char == 'T' {
                                    remaining_bytes -= 1;
                                    break;
                                }
                            }
                            // match 'LEN' ('T' was matched above)
                            let mut buffer = [0; 3];
                            f.read(&mut buffer)?;
                            // counter += 3;
                            remaining_bytes -= 3;
                            if buffer[0] as char == 'L'
                                && buffer[1] as char == 'E'
                                && buffer[2] as char == 'N'
                            {
                                // read short (16 bits = 2 bytes) for each type
                                for _i in 0..nr_types as usize {
                                    let mut buffer = [0; 2];
                                    f.read(&mut buffer)?;
                                    // counter += 2;
                                    remaining_bytes -= 2;
                                    let mut type_size: u16 = 0;
                                    type_size += (buffer[0] as u16) << 0;
                                    type_size += (buffer[1] as u16) << 8;
                                    tlen.push(type_size);
                                }
                                // skip pad bytes, read "STRC"
                                let mut buffer = [0; 1];
                                loop {
                                    f.read(&mut buffer)?;
                                    // counter += 1;
                                    if buffer[0] == 0 {
                                        // skip pad byte
                                        remaining_bytes -= 1;
                                    } else if buffer[0] as char == 'S' {
                                        remaining_bytes -= 1;
                                        break;
                                    }
                                }
                                // match 'TRC' ('S' was matched above)
                                let mut buffer = [0; 3];
                                f.read(&mut buffer)?;
                                // counter += 3;
                                remaining_bytes -= 3;
                                if buffer[0] as char == 'T'
                                    && buffer[1] as char == 'R'
                                    && buffer[2] as char == 'C'
                                {
                                    // nr_structs
                                    let mut buffer = [0; 4];
                                    f.read(&mut buffer)?;
                                    // counter += 4;
                                    remaining_bytes -= 4;
                                    let mut nr_structs: u32 = 0;
                                    nr_structs += (buffer[0] as u32) << 0;
                                    nr_structs += (buffer[1] as u32) << 8;
                                    nr_structs += (buffer[2] as u32) << 16;
                                    nr_structs += (buffer[3] as u32) << 24;
                                    for _s in 0..nr_structs as usize {
                                        // read two short values
                                        let mut buffer = [0; 2];
                                        f.read(&mut buffer)?;
                                        // counter += 2;
                                        remaining_bytes -= 2;
                                        let mut type_idx: u16 = 0;
                                        type_idx += (buffer[0] as u16) << 0;
                                        type_idx += (buffer[1] as u16) << 8;
                                        f.read(&mut buffer)?;
                                        // counter += 2;
                                        remaining_bytes -= 2;
                                        let mut short2: u16 = 0;
                                        short2 += (buffer[0] as u16) << 0;
                                        short2 += (buffer[1] as u16) << 8;
                                        dna_2_type_id.push(type_idx);
                                        let tuple_counter: usize = short2 as usize;
                                        for _t in 0..tuple_counter {
                                            // read two short values
                                            let mut buffer = [0; 2];
                                            f.read(&mut buffer)?;
                                            // counter += 2;
                                            remaining_bytes -= 2;
                                            f.read(&mut buffer)?;
                                            // counter += 2;
                                            remaining_bytes -= 2;
                                        }
                                    }
                                } else {
                                    println!("ERROR: \"STRC\" expected, \"S\"{:?} found", buffer)
                                }
                            } else {
                                println!("ERROR: \"TLEN\" expected, \"T\"{:?} found", buffer)
                            }
                        } else {
                            println!("ERROR: \"TYPE\" expected, \"T\"{:?} found", buffer)
                        }
                        // read remaining bytes
                        let mut buffer = vec![0; remaining_bytes];
                        f.read(&mut buffer)?;
                        // counter += remaining_bytes;
                    }
                } else {
                    // read len bytes
                    let mut buffer = vec![0; len as usize];
                    f.read(&mut buffer)?;
                    // counter += len as usize;
                    if code == String::from("OB") {
                        ob_count += 1;
                    }
                }
            }
            // println!("{} bytes read", counter);
        }
    }
    // then use the DNA
    let mut material_hm: HashMap<String, Blend279Material> = HashMap::with_capacity(ob_count);
    let mut object_to_world_hm: HashMap<String, Transform> = HashMap::with_capacity(ob_count);
    let mut builder: SceneDescriptionBuilder = SceneDescriptionBuilder::new();
    {
        let mut f = File::open(&args.path)?;
        let parent = args.path.parent().unwrap();
        // read exactly 12 bytes
        let mut counter: usize = 0;
        let mut buffer = [0; 12];
        f.read(&mut buffer)?;
        counter += 12;
        let mut blender_version: u32 = 0;
        if !decode_blender_header(&buffer, &mut blender_version, false) {
            println!("ERROR: Not a .blend file");
            println!("First 12 bytes:");
            println!("{:?}", buffer);
        } else {
            let mut data_following_mesh: bool = false;
            let mut data_following_object: bool = false;
            let mut is_smooth: bool = false;
            // Blender
            let mut loop_indices: Vec<u32> = Vec::with_capacity(4194304); // 2^22
            loop {
                // code
                let mut buffer = [0; 4];
                f.read(&mut buffer)?;
                counter += 4;
                let code = make_id(&buffer);
                // len
                let mut buffer = [0; 4];
                f.read(&mut buffer)?;
                counter += 4;
                let mut len: u32 = 0;
                len += (buffer[0] as u32) << 0;
                len += (buffer[1] as u32) << 8;
                len += (buffer[2] as u32) << 16;
                len += (buffer[3] as u32) << 24;
                // for now ignore the old entry
                let mut buffer = [0; 8];
                f.read(&mut buffer)?;
                counter += 8;
                // get SDNAnr
                let mut buffer = [0; 4];
                f.read(&mut buffer)?;
                counter += 4;
                let mut sdna_nr: u32 = 0;
                sdna_nr += (buffer[0] as u32) << 0;
                sdna_nr += (buffer[1] as u32) << 8;
                sdna_nr += (buffer[2] as u32) << 16;
                sdna_nr += (buffer[3] as u32) << 24;
                // get data len
                let mut buffer = [0; 4];
                f.read(&mut buffer)?;
                counter += 4;
                let mut data_len: u32 = 0;
                data_len += (buffer[0] as u32) << 0;
                data_len += (buffer[1] as u32) << 8;
                data_len += (buffer[2] as u32) << 16;
                data_len += (buffer[3] as u32) << 24;
                // are we done?
                if code == String::from("ENDB") {
                    break;
                }
                if code == String::from("DNA1") {
                    // read len bytes
                    let mut buffer = vec![0; len as usize];
                    f.read(&mut buffer)?;
                    counter += len as usize;
                    if data_following_object {
                        if base_name.starts_with("PbrtSphere") {
                            // store sphere values for later
                            let pbrt_sphere: PbrtSphere = PbrtSphere::new(
                                prop_radius as f32,
                                prop_zmin as f32,
                                prop_zmax as f32,
                                prop_phimax as f32,
                            );
                            spheres_hm.insert(base_name.clone(), pbrt_sphere);
                        } else if base_name.starts_with("PbrtCylinder") {
                            // store cylinder values for later
                            let pbrt_cylinder: PbrtCylinder = PbrtCylinder::new(
                                prop_radius as f32,
                                prop_zmin as f32,
                                prop_zmax as f32,
                                prop_phimax as f32,
                            );
                            cylinders_hm.insert(base_name.clone(), pbrt_cylinder);
                        } else if base_name.starts_with("PbrtDisk") {
                            // store disk values for later
                            let pbrt_disk: PbrtDisk = PbrtDisk::new(
                                prop_height as f32,
                                prop_radius as f32,
                                prop_innerradius as f32,
                                prop_phimax as f32,
                            );
                            disks_hm.insert(base_name.clone(), pbrt_disk);
                        }
                    }
                    if data_following_mesh {
                        if base_name.starts_with("PbrtSphere") {
                            // create sphere after mesh data
                            if let Some(o2w) = object_to_world_hm.get(&base_name) {
                                object_to_world = *o2w;
                            } else {
                                println!(
                                    "WARNING: looking up object_to_world by name ({:?}) failed",
                                    base_name
                                );
                            }
                            let world_to_object: Transform = Transform::inverse(&object_to_world);
                            if let Some(sphere) = spheres_hm.get(&base_name) {
                                builder.add_sphere(
                                    base_name.clone(),
                                    object_to_world,
                                    world_to_object,
                                    sphere.radius,
                                    sphere.zmin,
                                    sphere.zmax,
                                    sphere.phimax,
                                );
                            }
                        } else if base_name.starts_with("PbrtCylinder") {
                            // create cylinder after mesh data
                            if let Some(o2w) = object_to_world_hm.get(&base_name) {
                                object_to_world = *o2w;
                            } else {
                                println!(
                                    "WARNING: looking up object_to_world by name ({:?}) failed",
                                    base_name
                                );
                            }
                            let world_to_object: Transform = Transform::inverse(&object_to_world);
                            if let Some(cylinder) = cylinders_hm.get(&base_name) {
                                builder.add_cylinder(
                                    base_name.clone(),
                                    object_to_world,
                                    world_to_object,
                                    cylinder.radius,
                                    cylinder.zmin,
                                    cylinder.zmax,
                                    cylinder.phimax,
                                );
                            }
                        } else if base_name.starts_with("PbrtDisk") {
                            // create disk after mesh data
                            if let Some(o2w) = object_to_world_hm.get(&base_name) {
                                object_to_world = *o2w;
                            } else {
                                println!(
                                    "WARNING: looking up object_to_world by name ({:?}) failed",
                                    base_name
                                );
                            }
                            let world_to_object: Transform = Transform::inverse(&object_to_world);
                            if let Some(disk) = disks_hm.get(&base_name) {
                                builder.add_disk(
                                    base_name.clone(),
                                    object_to_world,
                                    world_to_object,
                                    disk.height,
                                    disk.radius,
                                    disk.innerradius,
                                    disk.phimax,
                                );
                            }
                        } else {
                            read_mesh(
                                &base_name,
                                &object_to_world_hm,
                                &mut object_to_world,
                                &p,
                                &n,
                                &mut uvs,
                                &loops,
                                vertex_indices.clone(),
                                vertex_colors.clone(),
                                is_smooth,
                                &mut builder,
                            );
                        }
                        vertex_indices.clear();
                        vertex_colors.clear();
                    }
                    // reset booleans
                    data_following_mesh = false;
                    data_following_object = false;
                    is_smooth = false;
                } else {
                    // read len bytes
                    let mut buffer = vec![0; len as usize];
                    f.read(&mut buffer)?;
                    counter += len as usize;
                    if code == String::from("IM") {
                        // IM
                        // println!("{} ({})", code, len);
                        // println!("  SDNAnr = {}", sdna_nr);
                        // v279: Image (len=1992) { ... }
                        // v280: Image (len=1440) { ... }
                        let mut skip_bytes: usize = 0;
                        // id
                        let mut id_name = String::new();
                        base_name = String::new();
                        for i in 32..(32 + 66) {
                            if buffer[i] == 0 {
                                break;
                            }
                            id_name.push(buffer[i] as char);
                            if i != 32 && i != 33 {
                                base_name.push(buffer[i] as char);
                            }
                        }
                        // println!("  id_name = {}", id_name);
                        // println!("  base_name = {}", base_name);
                        if blender_version < 280 {
                            skip_bytes += 120;
                        } else {
                            skip_bytes += 152;
                        }
                        // char name[1024]
                        let mut img_name = String::new();
                        for i in 0..1024 {
                            if buffer[skip_bytes + i] == 0 {
                                break;
                            }
                            img_name.push(buffer[skip_bytes + i] as char);
                        }
                        // skip_bytes += 1024;
                        if img_name.len() > 2 {
                            // println!("  img_name = {}", img_name);
                            let image_path: &Path = Path::new(&img_name);
                            // println!("  image_path = {:?}", image_path);
                            if let Some(img_ext) = image_path.extension() {
                                // println!("  img_ext = {:?}", img_ext);
                                if img_ext == "hdr" {
                                    if image_path.starts_with("//") {
                                        if let Ok(relative) = image_path.strip_prefix("//") {
                                            let canonicalized = parent
                                                .join(relative.clone())
                                                .canonicalize()
                                                .unwrap();
                                            println!("{:?}", canonicalized);
                                            hdr_path = canonicalized.into_os_string();
                                        }
                                    }
                                } else {
                                    if image_path.starts_with("//") {
                                        if let Ok(relative) = image_path.strip_prefix("//") {
                                            let canonicalized = parent
                                                .join(relative.clone())
                                                .canonicalize()
                                                .unwrap();
                                            println!("{:?}", canonicalized);
                                            texture_hm.insert(
                                                base_name.clone(),
                                                canonicalized.into_os_string(),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    } else if code == String::from("OB") {
                        if data_following_object {
                            if base_name.starts_with("PbrtSphere") {
                                // store sphere values for later
                                let pbrt_sphere: PbrtSphere = PbrtSphere::new(
                                    prop_radius as f32,
                                    prop_zmin as f32,
                                    prop_zmax as f32,
                                    prop_phimax as f32,
                                );
                                spheres_hm.insert(base_name.clone(), pbrt_sphere);
                            } else if base_name.starts_with("PbrtCylinder") {
                                // store cylinder values for later
                                let pbrt_cylinder: PbrtCylinder = PbrtCylinder::new(
                                    prop_radius as f32,
                                    prop_zmin as f32,
                                    prop_zmax as f32,
                                    prop_phimax as f32,
                                );
                                cylinders_hm.insert(base_name.clone(), pbrt_cylinder);
                            } else if base_name.starts_with("PbrtDisk") {
                                // store disk values for later
                                let pbrt_disk: PbrtDisk = PbrtDisk::new(
                                    prop_height as f32,
                                    prop_radius as f32,
                                    prop_innerradius as f32,
                                    prop_phimax as f32,
                                );
                                disks_hm.insert(base_name.clone(), pbrt_disk);
                            }
                        }
                        // OB
                        // println!("{} ({})", code, len);
                        // println!("  SDNAnr = {}", sdna_nr);
                        // v279: Object (len=1440) { ... }
                        // v280: Object (len=1408) { ... }
                        let mut skip_bytes: usize = 0;
                        // id
                        let mut id_name = String::new();
                        base_name = String::new();
                        for i in 32..(32 + 66) {
                            if buffer[i] == 0 {
                                break;
                            }
                            id_name.push(buffer[i] as char);
                            if i != 32 && i != 33 {
                                base_name.push(buffer[i] as char);
                            }
                        }
                        // println!("  id_name = {}", id_name);
                        // println!("  base_name = {}", base_name);
                        if blender_version < 280 {
                            skip_bytes += 120;
                        } else {
                            skip_bytes += 152;
                        }
                        // adt
                        skip_bytes += 8;
                        if blender_version < 280 {
                            // nothing there
                        } else {
                            // DrawDataList (len=16)
                            skip_bytes += 16;
                        }
                        // sculpt
                        skip_bytes += 8;
                        // type
                        // let mut ob_type: u16 = 0;
                        // ob_type += (buffer[skip_bytes] as u16) << 0;
                        // ob_type += (buffer[skip_bytes + 1] as u16) << 8;
                        skip_bytes += 2;
                        // match ob_type {
                        //     0 => println!("  ob_type = {}", "OB_EMPTY"),
                        //     1 => println!("  ob_type = {}", "OB_MESH"),
                        //     11 => println!("  ob_type = {}", "OB_CAMERA"),
                        //     _ => println!("  ob_type = {}", ob_type),
                        // }
                        // partype
                        skip_bytes += 2;
                        // par1, par2, par3
                        skip_bytes += 4 * 3;
                        // parsubstr[64]
                        skip_bytes += 64;
                        // parent, track, proxy, proxy_group, proxy_from
                        skip_bytes += 8 * 5;
                        // ipo
                        skip_bytes += 8;
                        if blender_version < 280 {
                            // bb
                            skip_bytes += 8;
                        } else {
                            // nothing there
                        }
                        // action, poselib, pose, data, gpd
                        skip_bytes += 8 * 5;
                        // v279: bAnimVizSettings (len=48)
                        // v280: bAnimVizSettings (len=32)
                        if blender_version < 280 {
                            skip_bytes += 48;
                        } else {
                            skip_bytes += 32;
                        }
                        // mpath
                        skip_bytes += 8;
                        if blender_version < 280 {
                            // ListBase * 4
                            skip_bytes += 16 * 4;
                        } else {
                            // _pad0
                            skip_bytes += 8;
                            // ListBase * 7
                            skip_bytes += 16 * 7;
                        }
                        // mode, restore_mode
                        skip_bytes += 4 * 2;
                        // mat, matbits
                        skip_bytes += 8 * 2;
                        // totcol, actcol
                        skip_bytes += 4 * 2;
                        // loc
                        skip_bytes += 4 * 3;
                        // dloc
                        skip_bytes += 4 * 3;
                        if blender_version < 280 {
                            // orig
                            skip_bytes += 4 * 3;
                        } else {
                            // nothing there
                        }
                        // size
                        skip_bytes += 4 * 3;
                        // dsize
                        skip_bytes += 4 * 3;
                        // dscale
                        skip_bytes += 4 * 3;
                        // rot
                        for _i in 0..3 {
                            let mut rot_buf: [u8; 4] = [0_u8; 4];
                            for i in 0..4 as usize {
                                rot_buf[i] = buffer[skip_bytes + i];
                            }
                            let _rot: f32 = unsafe { mem::transmute(rot_buf) };
                            // println!("  rot[{}] = {}", i, rot);
                            skip_bytes += 4;
                        }
                        //skip_bytes += 4 * 3;
                        // drot
                        skip_bytes += 4 * 3;
                        // quat
                        skip_bytes += 4 * 4;
                        // dquat
                        skip_bytes += 4 * 4;
                        // rotAxis
                        skip_bytes += 4 * 3;
                        // drotAxis
                        skip_bytes += 4 * 3;
                        // rotAngle
                        let mut rot_angle_buf: [u8; 4] = [0_u8; 4];
                        for i in 0..4 as usize {
                            rot_angle_buf[i] = buffer[skip_bytes + i];
                        }
                        let _rot_angle: f32 = unsafe { mem::transmute(rot_angle_buf) };
                        // println!("  rot_angle = {}", rot_angle);
                        skip_bytes += 4;
                        // drotAngle
                        skip_bytes += 4;
                        // obmat
                        let mut mat_values: [f32; 16] = [0.0_f32; 16];
                        for i in 0..4 {
                            for j in 0..4 {
                                let mut obmat_buf: [u8; 4] = [0_u8; 4];
                                for i in 0..4 as usize {
                                    obmat_buf[i] = buffer[skip_bytes + i];
                                }
                                let obmat: f32 = unsafe { mem::transmute(obmat_buf) };
                                // println!("  obmat[{}][{}] = {}", i, j, obmat);
                                mat_values[i * 4 + j] = obmat;
                                skip_bytes += 4;
                            }
                        }
                        object_to_world = Transform::new(
                            mat_values[0],
                            mat_values[4],
                            mat_values[8],
                            mat_values[12] * scale_length,
                            mat_values[1],
                            mat_values[5],
                            mat_values[9],
                            mat_values[13] * scale_length,
                            mat_values[2],
                            mat_values[6],
                            mat_values[10],
                            mat_values[14] * scale_length,
                            mat_values[3],
                            mat_values[7],
                            mat_values[11],
                            mat_values[15],
                        );
                        object_to_world_hm.insert(base_name.clone(), object_to_world);
                        // parentinv
                        for _i in 0..4 {
                            for _j in 0..4 {
                                let mut parentinv_buf: [u8; 4] = [0_u8; 4];
                                for i in 0..4 as usize {
                                    parentinv_buf[i] = buffer[skip_bytes + i];
                                }
                                let _parentinv: f32 = unsafe { mem::transmute(parentinv_buf) };
                                // println!("  parentinv[{}][{}] = {}", i, j, parentinv);
                                skip_bytes += 4;
                            }
                        }
                        // constinv
                        for _i in 0..4 {
                            for _j in 0..4 {
                                let mut constinv_buf: [u8; 4] = [0_u8; 4];
                                for i in 0..4 as usize {
                                    constinv_buf[i] = buffer[skip_bytes + i];
                                }
                                let _constinv: f32 = unsafe { mem::transmute(constinv_buf) };
                                // println!("  constinv[{}][{}] = {}", i, j, constinv);
                                skip_bytes += 4;
                            }
                        }
                        // imat
                        for _i in 0..4 {
                            for _j in 0..4 {
                                let mut imat_buf: [u8; 4] = [0_u8; 4];
                                for i in 0..4 as usize {
                                    imat_buf[i] = buffer[skip_bytes + i];
                                }
                                let _imat: f32 = unsafe { mem::transmute(imat_buf) };
                                // println!("  imat[{}][{}] = {}", i, j, imat);
                                skip_bytes += 4;
                            }
                        }
                        // imat_ren
                        for _i in 0..4 {
                            for _j in 0..4 {
                                let mut imat_ren_buf: [u8; 4] = [0_u8; 4];
                                for i in 0..4 as usize {
                                    imat_ren_buf[i] = buffer[skip_bytes + i];
                                }
                                let _imat_ren: f32 = unsafe { mem::transmute(imat_ren_buf) };
                                // println!("  imat_ren[{}][{}] = {}", i, j, imat_ren);
                                skip_bytes += 4;
                            }
                        }
                        // reset booleans
                        data_following_mesh = false;
                        data_following_object = true;
                        is_smooth = false;
                    } else if code == String::from("ME") {
                        if data_following_object {
                            if base_name.starts_with("PbrtSphere") {
                                // store sphere values for later
                                let pbrt_sphere: PbrtSphere = PbrtSphere::new(
                                    prop_radius as f32,
                                    prop_zmin as f32,
                                    prop_zmax as f32,
                                    prop_phimax as f32,
                                );
                                spheres_hm.insert(base_name.clone(), pbrt_sphere);
                            } else if base_name.starts_with("PbrtCylinder") {
                                // store cylinder values for later
                                let pbrt_cylinder: PbrtCylinder = PbrtCylinder::new(
                                    prop_radius as f32,
                                    prop_zmin as f32,
                                    prop_zmax as f32,
                                    prop_phimax as f32,
                                );
                                cylinders_hm.insert(base_name.clone(), pbrt_cylinder);
                            } else if base_name.starts_with("PbrtDisk") {
                                // store disk values for later
                                let pbrt_disk: PbrtDisk = PbrtDisk::new(
                                    prop_height as f32,
                                    prop_radius as f32,
                                    prop_innerradius as f32,
                                    prop_phimax as f32,
                                );
                                disks_hm.insert(base_name.clone(), pbrt_disk);
                            }
                        }
                        if data_following_mesh {
                            if base_name.starts_with("PbrtSphere") {
                                // create sphere after mesh data
                                if let Some(o2w) = object_to_world_hm.get(&base_name) {
                                    object_to_world = *o2w;
                                } else {
                                    println!(
                                        "WARNING: looking up object_to_world by name ({:?}) failed",
                                        base_name
                                    );
                                }
                                let world_to_object: Transform =
                                    Transform::inverse(&object_to_world);
                                if let Some(sphere) = spheres_hm.get(&base_name) {
                                    builder.add_sphere(
                                        base_name.clone(),
                                        object_to_world,
                                        world_to_object,
                                        sphere.radius,
                                        sphere.zmin,
                                        sphere.zmax,
                                        sphere.phimax,
                                    );
                                }
                            } else if base_name.starts_with("PbrtCylinder") {
                                // create cylinder after mesh data
                                if let Some(o2w) = object_to_world_hm.get(&base_name) {
                                    object_to_world = *o2w;
                                } else {
                                    println!(
                                        "WARNING: looking up object_to_world by name ({:?}) failed",
                                        base_name
                                    );
                                }
                                let world_to_object: Transform =
                                    Transform::inverse(&object_to_world);
                                if let Some(cylinder) = cylinders_hm.get(&base_name) {
                                    builder.add_cylinder(
                                        base_name.clone(),
                                        object_to_world,
                                        world_to_object,
                                        cylinder.radius,
                                        cylinder.zmin,
                                        cylinder.zmax,
                                        cylinder.phimax,
                                    );
                                }
                            } else if base_name.starts_with("PbrtDisk") {
                                // create disk after mesh data
                                if let Some(o2w) = object_to_world_hm.get(&base_name) {
                                    object_to_world = *o2w;
                                } else {
                                    println!(
                                        "WARNING: looking up object_to_world by name ({:?}) failed",
                                        base_name
                                    );
                                }
                                let world_to_object: Transform =
                                    Transform::inverse(&object_to_world);
                                if let Some(disk) = disks_hm.get(&base_name) {
                                    builder.add_disk(
                                        base_name.clone(),
                                        object_to_world,
                                        world_to_object,
                                        disk.height,
                                        disk.radius,
                                        disk.innerradius,
                                        disk.phimax,
                                    );
                                }
                            } else {
                                read_mesh(
                                    &base_name,
                                    &object_to_world_hm,
                                    &mut object_to_world,
                                    &p,
                                    &n,
                                    &mut uvs,
                                    &loops,
                                    vertex_indices.clone(),
                                    vertex_colors.clone(),
                                    is_smooth,
                                    &mut builder,
                                );
                            }
                            vertex_indices.clear();
                            vertex_colors.clear();
                        }
                        // ME
                        // println!("{} ({})", code, len);
                        // println!("  SDNAnr = {}", sdna_nr);
                        // Mesh (len=1416) { ... }
                        // let mut skip_bytes: usize = 0;
                        // id
                        let mut id_name = String::new();
                        base_name = String::new();
                        for i in 32..(32 + 66) {
                            if buffer[i] == 0 {
                                break;
                            }
                            id_name.push(buffer[i] as char);
                            if i != 32 && i != 33 {
                                base_name.push(buffer[i] as char);
                            }
                        }
                        // println!("  id_name = {}", id_name);
                        // println!("  base_name = {}", base_name);
                        // if blender_version < 280 {
                        //     skip_bytes += 120;
                        // } else {
                        //     skip_bytes += 152;
                        // }
                        // adt
                        // skip_bytes += 8;
                        // bb, ipo, key, mat, mselect, mpoly, mtpoly, mloop, mloopuv, mloopcol
                        // skip_bytes += 8 * 10;
                        // mface, mtface, tface, mvert, medge, dvert, mcol, texcomesh, edit_btmesh
                        // skip_bytes += 8 * 9;
                        // CustomData * 5
                        // skip_bytes += 208 * 5;
                        // totvert
                        // let mut totvert: u32 = 0;
                        // totvert += (buffer[skip_bytes] as u32) << 0;
                        // totvert += (buffer[skip_bytes + 1] as u32) << 8;
                        // totvert += (buffer[skip_bytes + 2] as u32) << 16;
                        // totvert += (buffer[skip_bytes + 3] as u32) << 24;
                        // println!("  totvert = {}", totvert);
                        // skip_bytes += 4;
                        // totedge
                        // let mut totedge: u32 = 0;
                        // totedge += (buffer[skip_bytes] as u32) << 0;
                        // totedge += (buffer[skip_bytes + 1] as u32) << 8;
                        // totedge += (buffer[skip_bytes + 2] as u32) << 16;
                        // totedge += (buffer[skip_bytes + 3] as u32) << 24;
                        // println!("  totedge = {}", totedge);
                        // skip_bytes += 4;
                        // totface
                        // let mut totface: u32 = 0;
                        // totface += (buffer[skip_bytes] as u32) << 0;
                        // totface += (buffer[skip_bytes + 1] as u32) << 8;
                        // totface += (buffer[skip_bytes + 2] as u32) << 16;
                        // totface += (buffer[skip_bytes + 3] as u32) << 24;
                        // println!("  totface = {}", totface);
                        // skip_bytes += 4;
                        // totselect
                        // let mut totselect: u32 = 0;
                        // totselect += (buffer[skip_bytes] as u32) << 0;
                        // totselect += (buffer[skip_bytes + 1] as u32) << 8;
                        // totselect += (buffer[skip_bytes + 2] as u32) << 16;
                        // totselect += (buffer[skip_bytes + 3] as u32) << 24;
                        // println!("  totselect = {}", totselect);
                        // skip_bytes += 4;
                        // totpoly
                        // let mut totpoly: u32 = 0;
                        // totpoly += (buffer[skip_bytes] as u32) << 0;
                        // totpoly += (buffer[skip_bytes + 1] as u32) << 8;
                        // totpoly += (buffer[skip_bytes + 2] as u32) << 16;
                        // totpoly += (buffer[skip_bytes + 3] as u32) << 24;
                        // println!("  totpoly = {}", totpoly);
                        // // skip_bytes += 4;
                        // totloop
                        // let mut totloop: u32 = 0;
                        // totloop += (buffer[skip_bytes] as u32) << 0;
                        // totloop += (buffer[skip_bytes + 1] as u32) << 8;
                        // totloop += (buffer[skip_bytes + 2] as u32) << 16;
                        // totloop += (buffer[skip_bytes + 3] as u32) << 24;
                        // println!("  totloop = {}", totloop);
                        // skip_bytes += 4;
                        // check tlen
                        // for n in 0..types.len() {
                        //     if types[n] == "CustomData" {
                        //         println!("  {:?} = types[{}] needs {} bytes", types[n], n, tlen[n]);
                        //     }
                        // }
                        // data_following_mesh
                        data_following_mesh = true;
                        // clear all Vecs
                        p.clear();
                        n.clear();
                        uvs.clear();
                        loops.clear();
                        vertex_indices.clear();
                        vertex_colors.clear();
                        loop_indices.clear();
                    } else if code == String::from("SC") {
                        // SC
                        // println!("{} ({})", code, len);
                        // println!("  SDNAnr = {}", sdna_nr);
                        // v279: Scene (len=5472) { ... }
                        // v280: Scene (len=6392) { ... }
                        let mut skip_bytes: usize = 0;
                        // v279: ID (len=120)
                        // v280: ID (len=152)
                        let mut id_name = String::new();
                        base_name = String::new();
                        for i in 32..(32 + 66) {
                            if buffer[i] == 0 {
                                break;
                            }
                            id_name.push(buffer[i] as char);
                            if i != 32 && i != 33 {
                                base_name.push(buffer[i] as char);
                            }
                        }
                        // println!("  id_name = {}", id_name);
                        // println!("  base_name = {}", base_name);
                        if blender_version < 280 {
                            skip_bytes += 120;
                        } else {
                            skip_bytes += 152;
                        }
                        // adt
                        skip_bytes += 8;
                        // camera, world, set
                        skip_bytes += 8 * 3;
                        // ListBase * 1
                        skip_bytes += 16 * 1;
                        // basact
                        skip_bytes += 8;
                        if blender_version < 280 {
                            // obedit
                            skip_bytes += 8;
                            // cursor
                            skip_bytes += 4 * 3;
                            // twcent
                            skip_bytes += 4 * 3;
                            // twmin
                            skip_bytes += 4 * 3;
                            // twmax
                            skip_bytes += 4 * 3;
                        } else {
                            // _pad1
                            skip_bytes += 8;
                            // View3DCursor (len=64)
                            skip_bytes += 64;
                        }
                        // lay, layact, lay_updated/_pad2[4]
                        skip_bytes += 4 * 3;
                        // flag
                        skip_bytes += 2;
                        // use_nodes
                        skip_bytes += 1;
                        // pad/_pad3
                        skip_bytes += 1;
                        // nodetree, ed, toolsettings, stats/_pad4
                        skip_bytes += 8 * 4;
                        // DisplaySafeAreas (len=32)
                        skip_bytes += 32;
                        // v279: RenderData (len=4432)
                        // v280: RenderData (len=4192)
                        let mut render_data_bytes: usize = 0;
                        // ImageFormatData (len=256)
                        skip_bytes += 256;
                        render_data_bytes += 256;
                        // avicodecdata
                        skip_bytes += 8;
                        render_data_bytes += 8;
                        if blender_version < 280 {
                            // qtcodecdata
                            skip_bytes += 8;
                            render_data_bytes += 8;
                            // QuicktimeCodecSettings (len=64)
                            skip_bytes += 64;
                            render_data_bytes += 64;
                        } else {
                            // nothing there
                        }
                        // FFMpegCodecData (len=88)
                        skip_bytes += 88;
                        render_data_bytes += 88;
                        // cfra
                        // let mut cfra: u32 = 0;
                        // cfra += (buffer[skip_bytes] as u32) << 0;
                        // cfra += (buffer[skip_bytes + 1] as u32) << 8;
                        // cfra += (buffer[skip_bytes + 2] as u32) << 16;
                        // cfra += (buffer[skip_bytes + 3] as u32) << 24;
                        // println!("    cfra = {}", cfra);
                        skip_bytes += 4;
                        render_data_bytes += 4;
                        // sfra
                        // let mut sfra: u32 = 0;
                        // sfra += (buffer[skip_bytes] as u32) << 0;
                        // sfra += (buffer[skip_bytes + 1] as u32) << 8;
                        // sfra += (buffer[skip_bytes + 2] as u32) << 16;
                        // sfra += (buffer[skip_bytes + 3] as u32) << 24;
                        // println!("    sfra = {}", sfra);
                        skip_bytes += 4;
                        render_data_bytes += 4;
                        // efra
                        // let mut efra: u32 = 0;
                        // efra += (buffer[skip_bytes] as u32) << 0;
                        // efra += (buffer[skip_bytes + 1] as u32) << 8;
                        // efra += (buffer[skip_bytes + 2] as u32) << 16;
                        // efra += (buffer[skip_bytes + 3] as u32) << 24;
                        // println!("    efra = {}", efra);
                        skip_bytes += 4;
                        render_data_bytes += 4;
                        // subframe
                        skip_bytes += 4;
                        render_data_bytes += 4;
                        // psfra, pefra, images, framapto
                        skip_bytes += 4 * 4;
                        render_data_bytes += 4 * 4;
                        // flag, threads
                        skip_bytes += 2 * 2;
                        render_data_bytes += 2 * 2;
                        // framelen, blurfac,
                        skip_bytes += 4 * 2;
                        render_data_bytes += 4 * 2;
                        if blender_version < 280 {
                            // edgeR, edgeG, edgeB
                            skip_bytes += 4 * 3;
                            render_data_bytes += 4 * 3;
                            // fullscreen, xplay, yplay, freqplay, depth, attrib
                            skip_bytes += 2 * 6;
                            render_data_bytes += 2 * 6;
                        } else {
                            // nothing there
                        }
                        // frame_step
                        skip_bytes += 4;
                        render_data_bytes += 4;
                        // stereomode, dimensionspreset
                        skip_bytes += 2 * 2;
                        render_data_bytes += 2 * 2;
                        if blender_version < 280 {
                            // filtertype
                            skip_bytes += 2;
                            render_data_bytes += 2;
                        } else {
                            // nothing there
                        }
                        // size in %
                        resolution_percentage = 0;
                        resolution_percentage += (buffer[skip_bytes] as u16) << 0;
                        resolution_percentage += (buffer[skip_bytes + 1] as u16) << 8;
                        // println!("resolution_percentage: {}", resolution_percentage);
                        skip_bytes += 2;
                        render_data_bytes += 2;
                        if blender_version < 280 {
                            // maximsize, pad6
                            skip_bytes += 2;
                            render_data_bytes += 2;
                        } else {
                            // nothing there
                        }
                        // pad6
                        skip_bytes += 2;
                        render_data_bytes += 2;
                        // xsch
                        let mut xsch: u32 = 0;
                        xsch += (buffer[skip_bytes] as u32) << 0;
                        xsch += (buffer[skip_bytes + 1] as u32) << 8;
                        xsch += (buffer[skip_bytes + 2] as u32) << 16;
                        xsch += (buffer[skip_bytes + 3] as u32) << 24;
                        // println!("    xsch = {}", xsch);
                        skip_bytes += 4;
                        render_data_bytes += 4;
                        resolution_x = xsch;
                        // ysch
                        let mut ysch: u32 = 0;
                        ysch += (buffer[skip_bytes] as u32) << 0;
                        ysch += (buffer[skip_bytes + 1] as u32) << 8;
                        ysch += (buffer[skip_bytes + 2] as u32) << 16;
                        ysch += (buffer[skip_bytes + 3] as u32) << 24;
                        // println!("    ysch = {}", ysch);
                        skip_bytes += 4;
                        render_data_bytes += 4;
                        resolution_y = ysch;
                        // skip remaining RenderData
                        if blender_version < 280 {
                            skip_bytes += 4432 - render_data_bytes;
                        } else {
                            skip_bytes += 4192 - render_data_bytes;
                        }
                        // AudioData (len=32)
                        skip_bytes += 32;
                        // ListBase * 2
                        skip_bytes += 16 * 2;
                        if blender_version < 280 {
                            // nothing there
                        } else {
                            // TransformOrientationSlot (16) * 4
                            skip_bytes += 16 * 4;
                        }
                        // sound_scene, playback_handle, sound_scrub_handle, speaker_handles, fps_info
                        skip_bytes += 8 * 5;
                        // depsgraph/depsgraph_hash
                        skip_bytes += 8;
                        if blender_version < 280 {
                            // pad1, theDag
                            skip_bytes += 8 * 2;
                            // dagflags, pad3
                            skip_bytes += 2 * 2;
                        } else {
                            // _pad7
                            skip_bytes += 4;
                        }
                        // active_keyingset
                        skip_bytes += 4;
                        // ListBase * 1
                        skip_bytes += 16 * 1;
                        if blender_version < 280 {
                            // GameFraming (len=16)
                            skip_bytes += 16;
                            // GameData (len=192)
                            skip_bytes += 192;
                        } else {
                            // nothing there
                        }
                        // v279: UnitSettings (len=8)
                        // v280: UnitSettings (len=16)
                        // scale_length
                        let mut scale_length_buf: [u8; 4] = [0_u8; 4];
                        for i in 0..4 as usize {
                            scale_length_buf[i] = buffer[skip_bytes + i];
                        }
                        scale_length = unsafe { mem::transmute(scale_length_buf) };
                        // println!("    scale_length = {}", scale_length);
                        // skip_bytes += 4;
                        // reset booleans
                        data_following_mesh = false;
                        data_following_object = false;
                        is_smooth = false;
                    } else if code == String::from("CA") {
                        // CA
                        // println!("{} ({})", code, len);
                        // println!("  SDNAnr = {}", sdna_nr);
                        // v279: Camera (len=248) { ... }
                        // v280: Camera (len=520) { ... }
                        let mut skip_bytes: usize = 0;
                        // v279: ID (len=120)
                        // v280: ID (len=152)
                        let mut id_name = String::new();
                        base_name = String::new();
                        for i in 32..(32 + 66) {
                            if buffer[i] == 0 {
                                break;
                            }
                            id_name.push(buffer[i] as char);
                            if i != 32 && i != 33 {
                                base_name.push(buffer[i] as char);
                            }
                        }
                        // println!("  id_name = {}", id_name);
                        // println!("  base_name = {}", base_name);
                        if blender_version < 280 {
                            skip_bytes += 120;
                        } else {
                            skip_bytes += 152;
                        }
                        // adt
                        skip_bytes += 8;
                        // type, dtx
                        skip_bytes += 2;
                        // flag
                        skip_bytes += 2;
                        // passepartalpha
                        skip_bytes += 4;
                        // clipsta
                        let mut clipsta_buf: [u8; 4] = [0_u8; 4];
                        for i in 0..4 as usize {
                            clipsta_buf[i] = buffer[skip_bytes + i];
                        }
                        let clipsta: f32 = unsafe { mem::transmute(clipsta_buf) };
                        // println!("  clipsta = {}", clipsta);
                        skip_bytes += 4;
                        // clipend
                        // let mut clipend_buf: [u8; 4] = [0_u8; 4];
                        // for i in 0..4 as usize {
                        //     clipend_buf[i] = buffer[skip_bytes + i];
                        // }
                        // let clipend: f32 = unsafe { mem::transmute(clipend_buf) };
                        // println!("  clipend = {}", clipend);
                        skip_bytes += 4;
                        // lens
                        let mut lens_buf: [u8; 4] = [0_u8; 4];
                        for i in 0..4 as usize {
                            lens_buf[i] = buffer[skip_bytes + i];
                        }
                        let lens: f32 = unsafe { mem::transmute(lens_buf) };
                        // println!("  lens = {}", lens);
                        skip_bytes += 4;
                        // ortho_scale
                        // let mut ortho_scale_buf: [u8; 4] = [0_u8; 4];
                        // for i in 0..4 as usize {
                        //     ortho_scale_buf[i] = buffer[skip_bytes + i];
                        // }
                        // let ortho_scale: f32 = unsafe { mem::transmute(ortho_scale_buf) };
                        // println!("  ortho_scale = {}", ortho_scale);
                        skip_bytes += 4;
                        // drawsize
                        // let mut drawsize_buf: [u8; 4] = [0_u8; 4];
                        // for i in 0..4 as usize {
                        //     drawsize_buf[i] = buffer[skip_bytes + i];
                        // }
                        // let drawsize: f32 = unsafe { mem::transmute(drawsize_buf) };
                        // println!("  drawsize = {}", drawsize);
                        skip_bytes += 4;
                        // sensor_x
                        let mut sensor_x_buf: [u8; 4] = [0_u8; 4];
                        for i in 0..4 as usize {
                            sensor_x_buf[i] = buffer[skip_bytes + i];
                        }
                        let sensor_x: f32 = unsafe { mem::transmute(sensor_x_buf) };
                        // println!("  sensor_x = {}", sensor_x);
                        skip_bytes += 4;
                        // sensor_y
                        let mut sensor_y_buf: [u8; 4] = [0_u8; 4];
                        for i in 0..4 as usize {
                            sensor_y_buf[i] = buffer[skip_bytes + i];
                        }
                        let sensor_y: f32 = unsafe { mem::transmute(sensor_y_buf) };
                        // println!("  sensor_y = {}", sensor_y);
                        // skip_bytes += 4;
                        // calculate angle_x and angle_y
                        angle_x = degrees(focallength_to_fov(lens, sensor_x) as Float);
                        angle_y = degrees(focallength_to_fov(lens, sensor_y) as Float);
                        // println!("  angle_x = {}", angle_x);
                        // println!("  angle_y = {}", angle_y);
                        // shiftx
                        // let mut shiftx_buf: [u8; 4] = [0_u8; 4];
                        // for i in 0..4 as usize {
                        //     shiftx_buf[i] = buffer[skip_bytes + i];
                        // }
                        // let shiftx: f32 = unsafe { mem::transmute(shiftx_buf) };
                        // println!("  shiftx = {}", shiftx);
                        // skip_bytes += 4;
                        // shifty
                        // let mut shifty_buf: [u8; 4] = [0_u8; 4];
                        // for i in 0..4 as usize {
                        //     shifty_buf[i] = buffer[skip_bytes + i];
                        // }
                        // let shifty: f32 = unsafe { mem::transmute(shifty_buf) };
                        // println!("  shifty = {}", shifty);
                        // skip_bytes += 4;
                        let cam: BlendCamera = BlendCamera {
                            lens,
                            angle_x,
                            angle_y,
                            clipsta,
                        };
                        camera_hm.insert(base_name.clone(), cam);
                        // reset booleans
                        data_following_mesh = false;
                        data_following_object = false;
                        is_smooth = false;
                    } else if code == String::from("MA") {
                        if data_following_object {
                            if base_name.starts_with("PbrtSphere") {
                                // store sphere values for later
                                let pbrt_sphere: PbrtSphere = PbrtSphere::new(
                                    prop_radius as f32,
                                    prop_zmin as f32,
                                    prop_zmax as f32,
                                    prop_phimax as f32,
                                );
                                spheres_hm.insert(base_name.clone(), pbrt_sphere);
                            } else if base_name.starts_with("PbrtCylinder") {
                                // store cylinder values for later
                                let pbrt_cylinder: PbrtCylinder = PbrtCylinder::new(
                                    prop_radius as f32,
                                    prop_zmin as f32,
                                    prop_zmax as f32,
                                    prop_phimax as f32,
                                );
                                cylinders_hm.insert(base_name.clone(), pbrt_cylinder);
                            } else if base_name.starts_with("PbrtDisk") {
                                // store disk values for later
                                let pbrt_disk: PbrtDisk = PbrtDisk::new(
                                    prop_height as f32,
                                    prop_radius as f32,
                                    prop_innerradius as f32,
                                    prop_phimax as f32,
                                );
                                disks_hm.insert(base_name.clone(), pbrt_disk);
                            }
                        }
                        if data_following_mesh {
                            if base_name.starts_with("PbrtSphere") {
                                // create sphere after mesh data
                                if let Some(o2w) = object_to_world_hm.get(&base_name) {
                                    object_to_world = *o2w;
                                } else {
                                    println!(
                                        "WARNING: looking up object_to_world by name ({:?}) failed",
                                        base_name
                                    );
                                }
                                let world_to_object: Transform =
                                    Transform::inverse(&object_to_world);
                                if let Some(sphere) = spheres_hm.get(&base_name) {
                                    builder.add_sphere(
                                        base_name.clone(),
                                        object_to_world,
                                        world_to_object,
                                        sphere.radius,
                                        sphere.zmin,
                                        sphere.zmax,
                                        sphere.phimax,
                                    );
                                }
                            } else if base_name.starts_with("PbrtCylinder") {
                                // create cylinder after mesh data
                                if let Some(o2w) = object_to_world_hm.get(&base_name) {
                                    object_to_world = *o2w;
                                } else {
                                    println!(
                                        "WARNING: looking up object_to_world by name ({:?}) failed",
                                        base_name
                                    );
                                }
                                let world_to_object: Transform =
                                    Transform::inverse(&object_to_world);
                                if let Some(cylinder) = cylinders_hm.get(&base_name) {
                                    builder.add_cylinder(
                                        base_name.clone(),
                                        object_to_world,
                                        world_to_object,
                                        cylinder.radius,
                                        cylinder.zmin,
                                        cylinder.zmax,
                                        cylinder.phimax,
                                    );
                                }
                            } else if base_name.starts_with("PbrtDisk") {
                                // create disk after mesh data
                                if let Some(o2w) = object_to_world_hm.get(&base_name) {
                                    object_to_world = *o2w;
                                } else {
                                    println!(
                                        "WARNING: looking up object_to_world by name ({:?}) failed",
                                        base_name
                                    );
                                }
                                let world_to_object: Transform =
                                    Transform::inverse(&object_to_world);
                                if let Some(disk) = disks_hm.get(&base_name) {
                                    builder.add_disk(
                                        base_name.clone(),
                                        object_to_world,
                                        world_to_object,
                                        disk.height,
                                        disk.radius,
                                        disk.innerradius,
                                        disk.phimax,
                                    );
                                }
                            } else {
                                read_mesh(
                                    &base_name,
                                    &object_to_world_hm,
                                    &mut object_to_world,
                                    &p,
                                    &n,
                                    &mut uvs,
                                    &loops,
                                    vertex_indices.clone(),
                                    vertex_colors.clone(),
                                    is_smooth,
                                    &mut builder,
                                );
                            }
                            vertex_indices.clear();
                            vertex_colors.clear();
                        }
                        // MA
                        // println!("{} ({})", code, len);
                        // println!("  SDNAnr = {}", sdna_nr);
                        // v279: Material (len=1528) { ... }
                        // v280: Material (len=320) { ... }
                        let mut skip_bytes: usize = 0;
                        // v279: ID (len=120)
                        // v280: ID (len=152)
                        let mut id_name = String::new();
                        base_name = String::new();
                        for i in 32..(32 + 66) {
                            if buffer[i] == 0 {
                                break;
                            }
                            id_name.push(buffer[i] as char);
                            if i != 32 && i != 33 {
                                base_name.push(buffer[i] as char);
                            }
                        }
                        // println!("  id_name = {}", id_name);
                        // println!("  base_name = {}", base_name);
                        if blender_version < 280 {
                            skip_bytes += 120;
                        } else {
                            skip_bytes += 152;
                        }
                        // adt
                        skip_bytes += 8;
                        if blender_version < 280 {
                            // material_type, flag
                            skip_bytes += 2 * 2;
                            // r
                            let mut r_buf: [u8; 4] = [0_u8; 4];
                            for i in 0..4 as usize {
                                r_buf[i] = buffer[skip_bytes + i];
                            }
                            let r: f32 = unsafe { mem::transmute(r_buf) };
                            // println!("  r = {}", r);
                            skip_bytes += 4;
                            // g
                            let mut g_buf: [u8; 4] = [0_u8; 4];
                            for i in 0..4 as usize {
                                g_buf[i] = buffer[skip_bytes + i];
                            }
                            let g: f32 = unsafe { mem::transmute(g_buf) };
                            // println!("  g = {}", g);
                            skip_bytes += 4;
                            // b
                            let mut b_buf: [u8; 4] = [0_u8; 4];
                            for i in 0..4 as usize {
                                b_buf[i] = buffer[skip_bytes + i];
                            }
                            let b: f32 = unsafe { mem::transmute(b_buf) };
                            // println!("  b = {}", b);
                            skip_bytes += 4;
                            // specr
                            let mut specr_buf: [u8; 4] = [0_u8; 4];
                            for i in 0..4 as usize {
                                specr_buf[i] = buffer[skip_bytes + i];
                            }
                            let specr: f32 = unsafe { mem::transmute(specr_buf) };
                            // println!("  specr = {}", specr);
                            skip_bytes += 4;
                            // specg
                            let mut specg_buf: [u8; 4] = [0_u8; 4];
                            for i in 0..4 as usize {
                                specg_buf[i] = buffer[skip_bytes + i];
                            }
                            let specg: f32 = unsafe { mem::transmute(specg_buf) };
                            // println!("  specg = {}", specg);
                            skip_bytes += 4;
                            // specb
                            let mut specb_buf: [u8; 4] = [0_u8; 4];
                            for i in 0..4 as usize {
                                specb_buf[i] = buffer[skip_bytes + i];
                            }
                            let specb: f32 = unsafe { mem::transmute(specb_buf) };
                            // println!("  specb = {}", specb);
                            skip_bytes += 4;
                            // mirr
                            let mut mirr_buf: [u8; 4] = [0_u8; 4];
                            for i in 0..4 as usize {
                                mirr_buf[i] = buffer[skip_bytes + i];
                            }
                            let mirr: f32 = unsafe { mem::transmute(mirr_buf) };
                            // println!("  mirr = {}", mirr);
                            skip_bytes += 4;
                            // mirg
                            let mut mirg_buf: [u8; 4] = [0_u8; 4];
                            for i in 0..4 as usize {
                                mirg_buf[i] = buffer[skip_bytes + i];
                            }
                            let mirg: f32 = unsafe { mem::transmute(mirg_buf) };
                            // println!("  mirg = {}", mirg);
                            skip_bytes += 4;
                            // mirb
                            let mut mirb_buf: [u8; 4] = [0_u8; 4];
                            for i in 0..4 as usize {
                                mirb_buf[i] = buffer[skip_bytes + i];
                            }
                            let mirb: f32 = unsafe { mem::transmute(mirb_buf) };
                            // println!("  mirb = {}", mirb);
                            skip_bytes += 4;
                            // ambr, ambg, ambb
                            skip_bytes += 4 * 3;
                            // amb
                            skip_bytes += 4;
                            // emit
                            let mut emit_buf: [u8; 4] = [0_u8; 4];
                            for i in 0..4 as usize {
                                emit_buf[i] = buffer[skip_bytes + i];
                            }
                            let emit: f32 = unsafe { mem::transmute(emit_buf) };
                            // println!("  emit = {}", emit);
                            skip_bytes += 4;
                            // ang (called "IOR" in Blender's UI)
                            let mut ang_buf: [u8; 4] = [0_u8; 4];
                            for i in 0..4 as usize {
                                ang_buf[i] = buffer[skip_bytes + i];
                            }
                            let ang: f32 = unsafe { mem::transmute(ang_buf) };
                            // println!("  ang = {}", ang);
                            skip_bytes += 4;
                            // spectra
                            skip_bytes += 4;
                            // ray_mirror
                            let mut ray_mirror_buf: [u8; 4] = [0_u8; 4];
                            for i in 0..4 as usize {
                                ray_mirror_buf[i] = buffer[skip_bytes + i];
                            }
                            let ray_mirror: f32 = unsafe { mem::transmute(ray_mirror_buf) };
                            // println!("  ray_mirror = {}", ray_mirror);
                            skip_bytes += 4;
                            // alpha, ref, spec, zoffs, add, translucency
                            skip_bytes += 4 * 6;
                            // VolumeSettings (len=88)
                            skip_bytes += 88;
                            // GameSettings (len=16)
                            skip_bytes += 16;
                            // 7 floats
                            skip_bytes += 4 * 7;
                            // 3 shorts
                            skip_bytes += 2 * 3;
                            // 2 chars
                            skip_bytes += 2;
                            // 2 floats
                            skip_bytes += 4 * 2;
                            // 2 shorts
                            skip_bytes += 2 * 2;
                            // 4 floats
                            skip_bytes += 4 * 4;
                            // 2 shorts
                            skip_bytes += 2 * 2;
                            // 4 ints
                            skip_bytes += 4 * 4;
                            // 4 shorts
                            skip_bytes += 2 * 4;
                            // 10 floats
                            skip_bytes += 4 * 10;
                            // 64 chars
                            skip_bytes += 64;
                            // 3 floats
                            skip_bytes += 4 * 3;
                            // 1 int
                            skip_bytes += 4;
                            // 4 chars
                            skip_bytes += 4;
                            // 3 shorts
                            skip_bytes += 2 * 3;
                            // 2 chars
                            skip_bytes += 2;
                            // 2 shorts
                            skip_bytes += 2 * 2;
                            // roughness
                            let mut roughness_buf: [u8; 4] = [0_u8; 4];
                            for i in 0..4 as usize {
                                roughness_buf[i] = buffer[skip_bytes + i];
                            }
                            let roughness: f32 = unsafe { mem::transmute(roughness_buf) };
                            // println!("  roughness = {}", roughness);
                            // skip_bytes += 4;
                            // Blend279Material
                            let mat: Blend279Material = Blend279Material {
                                r: r,
                                g: g,
                                b: b,
                                a: 1.0,
                                specr: specr,
                                specg: specg,
                                specb: specb,
                                mirr: mirr,
                                mirg: mirg,
                                mirb: mirb,
                                emit: emit,
                                ang: ang,
                                ray_mirror: ray_mirror,
                                roughness: roughness,
                            };
                            // println!("  mat[{:?}] = {:?}", base_name, mat);
                            material_hm.insert(base_name.clone(), mat);
                        } else {
                            // flag
                            skip_bytes += 2;
                            // _pad1
                            skip_bytes += 2;
                            // r
                            let mut r_buf: [u8; 4] = [0_u8; 4];
                            for i in 0..4 as usize {
                                r_buf[i] = buffer[skip_bytes + i];
                            }
                            let r: f32 = unsafe { mem::transmute(r_buf) };
                            // println!("  r = {}", r);
                            skip_bytes += 4;
                            // g
                            let mut g_buf: [u8; 4] = [0_u8; 4];
                            for i in 0..4 as usize {
                                g_buf[i] = buffer[skip_bytes + i];
                            }
                            let g: f32 = unsafe { mem::transmute(g_buf) };
                            // println!("  g = {}", g);
                            skip_bytes += 4;
                            // b
                            let mut b_buf: [u8; 4] = [0_u8; 4];
                            for i in 0..4 as usize {
                                b_buf[i] = buffer[skip_bytes + i];
                            }
                            let b: f32 = unsafe { mem::transmute(b_buf) };
                            // println!("  b = {}", b);
                            skip_bytes += 4;
                            // a
                            let mut a_buf: [u8; 4] = [0_u8; 4];
                            for i in 0..4 as usize {
                                a_buf[i] = buffer[skip_bytes + i];
                            }
                            let a: f32 = unsafe { mem::transmute(a_buf) };
                            // println!("  a = {}", a);
                            skip_bytes += 4;
                            // specr
                            let mut specr_buf: [u8; 4] = [0_u8; 4];
                            for i in 0..4 as usize {
                                specr_buf[i] = buffer[skip_bytes + i];
                            }
                            let specr: f32 = unsafe { mem::transmute(specr_buf) };
                            // println!("  specr = {}", specr);
                            skip_bytes += 4;
                            // specg
                            let mut specg_buf: [u8; 4] = [0_u8; 4];
                            for i in 0..4 as usize {
                                specg_buf[i] = buffer[skip_bytes + i];
                            }
                            let specg: f32 = unsafe { mem::transmute(specg_buf) };
                            // println!("  specg = {}", specg);
                            skip_bytes += 4;
                            // specb
                            let mut specb_buf: [u8; 4] = [0_u8; 4];
                            for i in 0..4 as usize {
                                specb_buf[i] = buffer[skip_bytes + i];
                            }
                            let specb: f32 = unsafe { mem::transmute(specb_buf) };
                            // println!("  specb = {}", specb);
                            skip_bytes += 4;
                            // alpha
                            skip_bytes += 4;
                            // ray_mirror
                            let mut ray_mirror_buf: [u8; 4] = [0_u8; 4];
                            for i in 0..4 as usize {
                                ray_mirror_buf[i] = buffer[skip_bytes + i];
                            }
                            let ray_mirror: f32 = unsafe { mem::transmute(ray_mirror_buf) };
                            // println!("  ray_mirror = {}", ray_mirror);
                            skip_bytes += 4;
                            // spec, gloss_mir, roughness, metallic
                            skip_bytes += 4 * 4;
                            // use_nodes
                            let _use_nodes: u8 = buffer[skip_bytes] as u8;
                            // println!("  use_nodes = {}", use_nodes);
                            // skip_bytes += 1;
                            let mat: Blend279Material = Blend279Material {
                                r: r,
                                g: g,
                                b: b,
                                a: a,
                                specr: specr,
                                specg: specg,
                                specb: specb,
                                mirr: 0.0,
                                mirg: 0.0,
                                mirb: 0.0,
                                emit: 0.0,
                                ang: 1.0,
                                ray_mirror: ray_mirror,
                                roughness: 0.0,
                            };
                            // println!("  mat[{:?}] = {:?}", base_name, mat);
                            material_hm.insert(base_name.clone(), mat);
                        }
                        // reset booleans
                        data_following_mesh = false;
                        data_following_object = false;
                        is_smooth = false;
                    } else if code == String::from("LA") {
                        // LA
                        // println!("{} ({})", code, len);
                        // println!("  SDNAnr = {}", sdna_nr);
                        // v279: Lamp (len=536) { ... }
                        // v280: Lamp (len=TODO) { ... }
                        let mut skip_bytes: usize = 0;
                        // id
                        let mut id_name = String::new();
                        base_name = String::new();
                        for i in 32..(32 + 66) {
                            if buffer[i] == 0 {
                                break;
                            }
                            id_name.push(buffer[i] as char);
                            if i != 32 && i != 33 {
                                base_name.push(buffer[i] as char);
                            }
                        }
                        // println!("  id_name = {}", id_name);
                        // println!("  base_name = {}", base_name);
                        if blender_version < 280 {
                            skip_bytes += 120;
                        } else {
                            skip_bytes += 152;
                        }
                        // adt
                        skip_bytes += 8;
                        // type
                        let mut la_type: u16 = 0;
                        la_type += (buffer[skip_bytes] as u16) << 0;
                        la_type += (buffer[skip_bytes + 1] as u16) << 8;
                        // println!("  la_type = {}", la_type);
                        skip_bytes += 2;
                        // flag
                        skip_bytes += 2;
                        // mode
                        skip_bytes += 4;
                        // colormodel, totex
                        skip_bytes += 2 * 2;
                        // r
                        let mut r_buf: [u8; 4] = [0_u8; 4];
                        for i in 0..4 as usize {
                            r_buf[i] = buffer[skip_bytes + i];
                        }
                        let r: f32 = unsafe { mem::transmute(r_buf) };
                        // println!("  r = {}", r);
                        skip_bytes += 4;
                        // g
                        let mut g_buf: [u8; 4] = [0_u8; 4];
                        for i in 0..4 as usize {
                            g_buf[i] = buffer[skip_bytes + i];
                        }
                        let g: f32 = unsafe { mem::transmute(g_buf) };
                        // println!("  g = {}", g);
                        skip_bytes += 4;
                        // b
                        let mut b_buf: [u8; 4] = [0_u8; 4];
                        for i in 0..4 as usize {
                            b_buf[i] = buffer[skip_bytes + i];
                        }
                        let b: f32 = unsafe { mem::transmute(b_buf) };
                        // println!("  b = {}", b);
                        skip_bytes += 4;
                        // k, shdwr, shdwg, shdwb, shdwpad
                        skip_bytes += 4 * 5;
                        // energy
                        let mut energy_buf: [u8; 4] = [0_u8; 4];
                        for i in 0..4 as usize {
                            energy_buf[i] = buffer[skip_bytes + i];
                        }
                        let energy: f32 = unsafe { mem::transmute(energy_buf) };
                        // println!("  energy = {}", energy);
                        // skip_bytes += 4;
                        // check light type
                        if la_type == 0 {
                            // LA_LOCAL
                            if let Some(o2w) = object_to_world_hm.get(&base_name) {
                                object_to_world = *o2w;
                            } else {
                                println!(
                                    "WARNING: looking up object_to_world by name ({:?}) failed",
                                    base_name
                                );
                            }
                            let l: Spectrum = Spectrum::rgb(r, g, b);
                            // point light
                            builder.add_point_light(object_to_world, l, args.light_scale * energy);
                        } else if la_type == 1 {
                            // LA_SUN
                            if let Some(o2w) = object_to_world_hm.get(&base_name) {
                                object_to_world = *o2w;
                            } else {
                                println!(
                                    "WARNING: looking up object_to_world by name ({:?}) failed",
                                    base_name
                                );
                            }
                            let l: Spectrum = Spectrum::rgb(r, g, b);
                            // distant light
                            builder.add_distant_light(
                                object_to_world,
                                l,
                                args.light_scale * energy,
                            );
                        } else {
                            println!("WARNING: la_type = {} not supported (yet)", la_type);
                        }
                    } else if code == String::from("DATA") {
                        // DATA
                        if data_following_mesh {
                            // type_id
                            let type_id: usize = dna_2_type_id[sdna_nr as usize] as usize;
                            if types[type_id] == "MPoly" {
                                // println!("{}[{}] ({})", code, data_len, len);
                                // println!("  SDNAnr = {}", sdna_nr);
                                // println!("  {} ({})", types[type_id], tlen[type_id]);
                                let mut skip_bytes: usize = 0;
                                for _p in 0..data_len {
                                    // println!("  {}:", p + 1);
                                    // loopstart
                                    let mut loopstart: u32 = 0;
                                    loopstart += (buffer[skip_bytes] as u32) << 0;
                                    loopstart += (buffer[skip_bytes + 1] as u32) << 8;
                                    loopstart += (buffer[skip_bytes + 2] as u32) << 16;
                                    loopstart += (buffer[skip_bytes + 3] as u32) << 24;
                                    // println!("    loopstart = {}", loopstart);
                                    skip_bytes += 4;
                                    // totloop
                                    let mut totloop: u32 = 0;
                                    totloop += (buffer[skip_bytes] as u32) << 0;
                                    totloop += (buffer[skip_bytes + 1] as u32) << 8;
                                    totloop += (buffer[skip_bytes + 2] as u32) << 16;
                                    totloop += (buffer[skip_bytes + 3] as u32) << 24;
                                    // println!("    totloop = {}", totloop);
                                    skip_bytes += 4;
                                    // mat_nr
                                    // let mut mat_nr: u16 = 0;
                                    // mat_nr += (buffer[skip_bytes] as u16) << 0;
                                    // mat_nr += (buffer[skip_bytes + 1] as u16) << 8;
                                    // println!("    mat_nr = {}", mat_nr);
                                    skip_bytes += 2;
                                    // flag
                                    let flag: u8 = buffer[skip_bytes];
                                    // println!("    flag = {}", flag);
                                    is_smooth = flag % 2 == 1;
                                    // println!("    is_smooth = {}", is_smooth);
                                    skip_bytes += 1;
                                    // pad
                                    // println!("    pad = {}", buffer[skip_bytes]);
                                    skip_bytes += 1;
                                    // PBRT
                                    if totloop == 3_u32 {
                                        loops.push(totloop as u8);
                                        // triangle
                                        for i in 0..3 {
                                            vertex_indices.push(
                                                loop_indices[(loopstart + i) as usize] as u32,
                                            );
                                        }
                                    } else if totloop == 4_u32 {
                                        loops.push(totloop as u8);
                                        // quads
                                        vertex_indices
                                            .push(loop_indices[(loopstart + 0) as usize] as u32);
                                        vertex_indices
                                            .push(loop_indices[(loopstart + 1) as usize] as u32);
                                        vertex_indices
                                            .push(loop_indices[(loopstart + 2) as usize] as u32);
                                        vertex_indices
                                            .push(loop_indices[(loopstart + 0) as usize] as u32);
                                        vertex_indices
                                            .push(loop_indices[(loopstart + 2) as usize] as u32);
                                        vertex_indices
                                            .push(loop_indices[(loopstart + 3) as usize] as u32);
                                    } else {
                                        println!(
                                            "WARNING: quads or triangles expected (totloop = {}): {:?}",
                                            totloop, base_name
                                        )
                                    }
                                }
                            // println!("  vertex_indices = {:?}", vertex_indices);
                            } else if types[type_id] == "MVert" {
                                // println!("{}[{}] ({})", code, data_len, len);
                                // println!("  SDNAnr = {}", sdna_nr);
                                // println!("  {} ({})", types[type_id], tlen[type_id]);
                                let mut skip_bytes: usize = 0;
                                let factor: f32 = 1.0 / 32767.0;
                                let mut coords: [f32; 3] = [0.0_f32; 3];
                                for _v in 0..data_len {
                                    // println!("  {}:", v + 1);
                                    // co
                                    for i in 0..3 {
                                        let mut co_buf: [u8; 4] = [0_u8; 4];
                                        for b in 0..4 as usize {
                                            co_buf[b] = buffer[skip_bytes + b];
                                        }
                                        let co: f32 = unsafe { mem::transmute(co_buf) };
                                        // println!("    co[{}] = {}", i, co);
                                        coords[i] = co;
                                        skip_bytes += 4;
                                    }
                                    p.push(Point3f {
                                        x: (coords[0] * scale_length) as Float,
                                        y: (coords[1] * scale_length) as Float,
                                        z: (coords[2] * scale_length) as Float,
                                    });
                                    // no
                                    for i in 0..3 {
                                        let mut no: i16 = 0;
                                        no += (buffer[skip_bytes] as i16) << 0;
                                        no += (buffer[skip_bytes + 1] as i16) << 8;
                                        let nof: f32 = no as f32 * factor;
                                        // println!("    no[{}] = {}", i, nof);
                                        coords[i] = nof;
                                        skip_bytes += 2;
                                    }
                                    n.push(Normal3f {
                                        x: coords[0] as Float,
                                        y: coords[1] as Float,
                                        z: coords[2] as Float,
                                    });
                                    // flag
                                    // println!("    flag = {}", buffer[skip_bytes]);
                                    skip_bytes += 1;
                                    // bweight
                                    // println!("    bweight = {}", buffer[skip_bytes]);
                                    skip_bytes += 1;
                                }
                            // for v in 0..data_len as usize {
                            //     println!("  {}:", v + 1);
                            //     println!("    co: {:?}", p[v]);
                            //     println!("    no: {:?}", n[v]);
                            // }
                            } else if types[type_id] == "MLoop" {
                                // println!("{}[{}] ({})", code, data_len, len);
                                // println!("  SDNAnr = {}", sdna_nr);
                                // println!("  {} ({})", types[type_id], tlen[type_id]);
                                let mut skip_bytes: usize = 0;
                                for _l in 0..data_len {
                                    // println!("  {}:", l + 1);
                                    // v
                                    let mut v: u32 = 0;
                                    v += (buffer[skip_bytes] as u32) << 0;
                                    v += (buffer[skip_bytes + 1] as u32) << 8;
                                    v += (buffer[skip_bytes + 2] as u32) << 16;
                                    v += (buffer[skip_bytes + 3] as u32) << 24;
                                    // println!("    v = {}", v);
                                    loop_indices.push(v);
                                    skip_bytes += 4;
                                    // e
                                    // let mut e: u32 = 0;
                                    // e += (buffer[skip_bytes] as u32) << 0;
                                    // e += (buffer[skip_bytes + 1] as u32) << 8;
                                    // e += (buffer[skip_bytes + 2] as u32) << 16;
                                    // e += (buffer[skip_bytes + 3] as u32) << 24;
                                    // println!("    e = {}", e);
                                    skip_bytes += 4;
                                }
                            // println!("    loop_indices = {:?}", loop_indices);
                            } else if types[type_id] == "MLoopUV" {
                                // println!("{}[{}] ({})", code, data_len, len);
                                // println!("  SDNAnr = {}", sdna_nr);
                                // println!("  {} ({})", types[type_id], tlen[type_id]);
                                let mut skip_bytes: usize = 0;
                                let mut coords: [f32; 2] = [0.0_f32; 2];
                                for _l in 0..data_len {
                                    // println!("  {}:", l + 1);
                                    // float uv[2]
                                    for i in 0..2 {
                                        let mut uv_buf: [u8; 4] = [0_u8; 4];
                                        for b in 0..4 as usize {
                                            uv_buf[b] = buffer[skip_bytes + b];
                                        }
                                        let uv: f32 = unsafe { mem::transmute(uv_buf) };
                                        // println!("    uv[{}] = {}", i, uv);
                                        coords[i] = uv;
                                        skip_bytes += 4;
                                    }
                                    uvs.push(Point2f {
                                        x: coords[0] as Float,
                                        y: coords[1] as Float,
                                    });
                                    // int flag
                                    skip_bytes += 4;
                                }
                            // for l in 0..data_len as usize {
                            //     println!("  {}:", l + 1);
                            //     println!("    uv: {:?}", uvs[l]);
                            // }
                            } else if types[type_id] == "MLoopCol" {
                                // println!("{}[{}] ({})", code, data_len, len);
                                // println!("  SDNAnr = {}", sdna_nr);
                                // println!("  {} ({})", types[type_id], tlen[type_id]);
                                // println!("  base_name = {}", base_name);
                                let mut skip_bytes: usize = 0;
                                for _l in 0..data_len {
                                    // r
                                    let mut red: u8 = 0;
                                    red += buffer[skip_bytes];
                                    skip_bytes += 1;
                                    // g
                                    let mut green: u8 = 0;
                                    green += buffer[skip_bytes];
                                    skip_bytes += 1;
                                    // b
                                    let mut blue: u8 = 0;
                                    blue += buffer[skip_bytes];
                                    skip_bytes += 1;
                                    // a
                                    let mut alpha: u8 = 0;
                                    alpha += buffer[skip_bytes];
                                    skip_bytes += 1;
                                    // println!("{}: {}, {}, {}, {}", l, red, green, blue, alpha);
                                    vertex_colors.push(red);
                                    vertex_colors.push(green);
                                    vertex_colors.push(blue);
                                    vertex_colors.push(alpha);
                                }
                                // println!("vertex_colors: {:?}", vertex_colors);
                            }
                        } else if data_following_object {
                            // type_id
                            let type_id: usize = dna_2_type_id[sdna_nr as usize] as usize;
                            if types[type_id] == "IDProperty" {
                                // println!("{}[{}] ({})", code, data_len, len);
                                // println!("  SDNAnr = {}", sdna_nr);
                                // println!("  {} ({})", types[type_id], tlen[type_id]);
                                let mut skip_bytes: usize = 0;
                                for _p in 0..data_len {
                                    // println!("  {}:", p + 1);
                                    // next
                                    skip_bytes += 8;
                                    // prev
                                    skip_bytes += 8;
                                    // type
                                    let prop_type: u8 = buffer[skip_bytes] as u8;
                                    // println!("  prop_type = {}", prop_type);
                                    skip_bytes += 1;
                                    if prop_type == 8_u8 {
                                        // IDP_DOUBLE (see DNA_ID.h)
                                        // subtype
                                        skip_bytes += 1;
                                        // flag
                                        skip_bytes += 2;
                                        // char name[64]
                                        let mut prop_name = String::new();
                                        for i in 0..64 {
                                            if buffer[skip_bytes + i] == 0 {
                                                break;
                                            }
                                            prop_name.push(buffer[skip_bytes + i] as char);
                                        }
                                        // if prop_name.len() > 0 {
                                        //     println!(
                                        //         "  prop_name[{}] = {}",
                                        //         prop_name.len(),
                                        //         prop_name
                                        //     );
                                        // }
                                        skip_bytes += 64;
                                        // saved
                                        skip_bytes += 4;
                                        // v279: data (len=32)
                                        // v280: data (len=32)
                                        skip_bytes += 8;
                                        // group
                                        skip_bytes += 16;
                                        // val
                                        // val2
                                        let mut val_buf: [u8; 8] = [0_u8; 8];
                                        for b in 0..8 as usize {
                                            val_buf[b] = buffer[skip_bytes + b];
                                        }
                                        let prop_val: f64 = unsafe { mem::transmute(val_buf) };
                                        // println!("  prop_val = {}", prop_val);
                                        skip_bytes += 8;
                                        if base_name.starts_with("PbrtSphere")
                                            || base_name.starts_with("PbrtCylinder")
                                            || base_name.starts_with("PbrtDisk")
                                        {
                                            // println!(
                                            //     "  {}.{} = {}",
                                            //     base_name, prop_name, prop_val
                                            // );
                                            if prop_name == String::from("height") {
                                                prop_height = prop_val;
                                            }
                                            if prop_name == String::from("radius") {
                                                prop_radius = prop_val;
                                            }
                                            if prop_name == String::from("innerradius") {
                                                prop_innerradius = prop_val;
                                            }
                                            if prop_name == String::from("zmin") {
                                                prop_zmin = prop_val;
                                            }
                                            if prop_name == String::from("zmax") {
                                                prop_zmax = prop_val;
                                            }
                                            if prop_name == String::from("phimax") {
                                                prop_phimax = prop_val;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        if data_following_object {
                            if base_name.starts_with("PbrtSphere") {
                                // store sphere values for later
                                let pbrt_sphere: PbrtSphere = PbrtSphere::new(
                                    prop_radius as f32,
                                    prop_zmin as f32,
                                    prop_zmax as f32,
                                    prop_phimax as f32,
                                );
                                spheres_hm.insert(base_name.clone(), pbrt_sphere);
                            } else if base_name.starts_with("PbrtCylinder") {
                                // store cylinder values for later
                                let pbrt_cylinder: PbrtCylinder = PbrtCylinder::new(
                                    prop_radius as f32,
                                    prop_zmin as f32,
                                    prop_zmax as f32,
                                    prop_phimax as f32,
                                );
                                cylinders_hm.insert(base_name.clone(), pbrt_cylinder);
                            } else if base_name.starts_with("PbrtDisk") {
                                // store disk values for later
                                let pbrt_disk: PbrtDisk = PbrtDisk::new(
                                    prop_height as f32,
                                    prop_radius as f32,
                                    prop_innerradius as f32,
                                    prop_phimax as f32,
                                );
                                disks_hm.insert(base_name.clone(), pbrt_disk);
                            }
                        }
                        if data_following_mesh {
                            if base_name.starts_with("PbrtSphere") {
                                // create sphere after mesh data
                                if let Some(o2w) = object_to_world_hm.get(&base_name) {
                                    object_to_world = *o2w;
                                } else {
                                    println!(
                                        "WARNING: looking up object_to_world by name ({:?}) failed",
                                        base_name
                                    );
                                }
                                let world_to_object: Transform =
                                    Transform::inverse(&object_to_world);
                                if let Some(sphere) = spheres_hm.get(&base_name) {
                                    builder.add_sphere(
                                        base_name.clone(),
                                        object_to_world,
                                        world_to_object,
                                        sphere.radius,
                                        sphere.zmin,
                                        sphere.zmax,
                                        sphere.phimax,
                                    );
                                }
                            } else if base_name.starts_with("PbrtCylinder") {
                                // create cylinder after mesh data
                                if let Some(o2w) = object_to_world_hm.get(&base_name) {
                                    object_to_world = *o2w;
                                } else {
                                    println!(
                                        "WARNING: looking up object_to_world by name ({:?}) failed",
                                        base_name
                                    );
                                }
                                let world_to_object: Transform =
                                    Transform::inverse(&object_to_world);
                                if let Some(cylinder) = cylinders_hm.get(&base_name) {
                                    builder.add_cylinder(
                                        base_name.clone(),
                                        object_to_world,
                                        world_to_object,
                                        cylinder.radius,
                                        cylinder.zmin,
                                        cylinder.zmax,
                                        cylinder.phimax,
                                    );
                                }
                            } else if base_name.starts_with("PbrtDisk") {
                                // create disk after mesh data
                                if let Some(o2w) = object_to_world_hm.get(&base_name) {
                                    object_to_world = *o2w;
                                } else {
                                    println!(
                                        "WARNING: looking up object_to_world by name ({:?}) failed",
                                        base_name
                                    );
                                }
                                let world_to_object: Transform =
                                    Transform::inverse(&object_to_world);
                                if let Some(disk) = disks_hm.get(&base_name) {
                                    builder.add_disk(
                                        base_name.clone(),
                                        object_to_world,
                                        world_to_object,
                                        disk.height,
                                        disk.radius,
                                        disk.innerradius,
                                        disk.phimax,
                                    );
                                }
                            } else {
                                read_mesh(
                                    &base_name,
                                    &object_to_world_hm,
                                    &mut object_to_world,
                                    &p,
                                    &n,
                                    &mut uvs,
                                    &loops,
                                    vertex_indices.clone(),
                                    vertex_colors.clone(),
                                    is_smooth,
                                    &mut builder,
                                );
                            }
                            vertex_indices.clear();
                            vertex_colors.clear();
                        }
                        // reset booleans
                        data_following_mesh = false;
                        data_following_object = false;
                        is_smooth = false;
                    }
                    if code != String::from("DATA")
                        && code != String::from("REND")
                        && code != String::from("TEST")
                    {
                        let type_id: usize = dna_2_type_id[sdna_nr as usize] as usize;
                        if len != tlen[type_id] as u32 {
                            println!("WARNING: {} ({} != {})", code, len, tlen[type_id]);
                        }
                    }
                }
            }
            println!("{} bytes read", counter);
        }
    }
    // use HDR image if one was found
    if !hdr_path.is_empty() {
        let axis: Vector3f = Vector3f {
            x: 0.0 as Float,
            y: 0.0 as Float,
            z: 1.0 as Float,
        };
        let light_to_world: Transform = Transform::rotate(180.0 as Float, &axis);
        builder.add_hdr_light(
            light_to_world,
            String::from(hdr_path.to_str().unwrap()),
            args.light_scale,
        );
    }
    let scene_description: SceneDescription = builder.finalize();
    let mut render_options: RenderOptions = RenderOptions::new(
        scene_description,
        &material_hm,
        &texture_hm,
        args.light_scale as Float,
    );
    assert!(render_options.shapes.len() == render_options.shape_lights.len());
    for shape_idx in 0..render_options.shapes.len() {
        let shape = &render_options.shapes[shape_idx];
        let shape_material = &render_options.shape_materials[shape_idx];
        let shape_light = &render_options.shape_lights[shape_idx];
        let geo_prim = Arc::new(Primitive::Geometric(Box::new(GeometricPrimitive::new(
            shape.clone(),
            Some(shape_material.clone()),
            shape_light.clone(),
            None,
        ))));
        render_options.primitives.push(geo_prim.clone());
    }
    println!("number of lights = {:?}", render_options.lights.len());
    println!(
        "number of primitives = {:?}",
        render_options.primitives.len()
    );
    let mut pos = Point3f {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };
    let mut look = Point3f {
        x: 1.0,
        y: 0.0,
        z: 0.0,
    };
    let mut up = Vector3f {
        x: 0.0,
        y: 0.0,
        z: 1.0,
    };
    base_name = String::from("Camera");
    if let Some(camera_name) = args.camera_name {
        base_name = camera_name.clone();
    }
    if let Some(o2w) = object_to_world_hm.get(&base_name) {
        pos = Point3f {
            x: o2w.m.m[0][3],
            y: o2w.m.m[1][3],
            z: o2w.m.m[2][3],
        };
        let forwards: Point3f = Point3f {
            x: -o2w.m.m[0][2] * scale_length,
            y: -o2w.m.m[1][2] * scale_length,
            z: -o2w.m.m[2][2] * scale_length,
        };
        look = pos + forwards;
        up = Vector3f {
            x: o2w.m.m[0][1],
            y: o2w.m.m[1][1],
            z: o2w.m.m[2][1],
        };
    } else {
        println!(
            "WARNING: looking up object_to_world by name ({:?}) failed",
            base_name
        );
    }
    let t: Transform = Transform::scale(-1.0, 1.0, 1.0) * Transform::look_at(&pos, &look, &up);
    let it: Transform = Transform {
        m: t.m_inv.clone(),
        m_inv: t.m.clone(),
    };
    let animated_cam_to_world: AnimatedTransform = AnimatedTransform::new(&it, 0.0, &it, 1.0);
    let aspect: Float = resolution_x as Float / resolution_y as Float;
    let mut fov: Float;
    let mut clipsta: Float = 0.0;
    if aspect > 1.0 {
        fov = angle_y;
    } else {
        fov = angle_x;
    }
    if let Some(cam) = camera_hm.get(&base_name) {
        // overwrite fov
        if aspect > 1.0 {
            // fov = 2.0 as Float * degrees((16.0 as Float / (aspect * cam.lens)).atan());
            fov = angle_x / 2.0;
        } else {
            fov = 2.0 as Float * degrees(((aspect * 16.0 as Float) / cam.lens).atan());
        }
        clipsta = cam.clipsta;
        // println!("fov[{}] overwritten", fov);
        // println!("clipsta[{}] overwritten", clipsta);
    }
    let frame: Float = resolution_x as Float / resolution_y as Float;
    let mut screen: Bounds2f = Bounds2f::default();
    if frame > 1.0 {
        screen.p_min.x = -frame;
        screen.p_max.x = frame;
        screen.p_min.y = -1.0;
        screen.p_max.y = 1.0;
    } else {
        screen.p_min.x = -1.0;
        screen.p_max.x = 1.0;
        screen.p_min.y = -1.0 / frame;
        screen.p_max.y = 1.0 / frame;
    }
    let render_x: u32 = resolution_x * resolution_percentage as u32 / 100_u32;
    let render_y: u32 = resolution_y * resolution_percentage as u32 / 100_u32;
    println!(
        "{}x{} [{}%] = {}x{}",
        resolution_x, resolution_y, resolution_percentage, render_x, render_y
    );
    if let Some(integrator_name) = args.integrator {
        let mut integrator_params: ParamSet = ParamSet::default();
        if integrator_name == "mlt" {
            integrator_params.add_int(String::from("maxdepth"), args.max_depth as i32);
            // MLT
            integrator_params.add_int(
                String::from("bootstrapsamples"),
                args.bootstrap_samples as i32,
            );
            integrator_params.add_int(String::from("chains"), args.chains as i32);
            integrator_params.add_int(
                String::from("mutationsperpixel"),
                args.mutations_per_pixel as i32,
            );
            integrator_params.add_float(
                String::from("largestepprobability"),
                args.step_probability as Float,
            );
            integrator_params.add_float(String::from("sigma"), args.sigma as Float);
        } else if integrator_name == "sppm" {
            integrator_params.add_int(String::from("maxdepth"), args.max_depth as i32);
            // SPPM
            integrator_params.add_int(String::from("imagewritefrequency"), args.write_frequency);
        } else {
            integrator_params.add_int(String::from("maxdepth"), args.max_depth as i32);
        }
        let some_integrator: Option<Box<Integrator>> = make_integrator(
            &integrator_name,
            2.0 as Float,
            render_x as i32,
            render_y as i32,
            fov,
            clipsta,
            animated_cam_to_world,
            args.samples as i32,
            integrator_params,
        );
        if let Some(mut integrator) = some_integrator {
            let scene = make_scene(&render_options.primitives, render_options.lights);
            let num_threads: u8 = num_cpus::get() as u8;
            integrator.render(&scene, num_threads);
        } else {
            panic!("Unable to create integrator.");
        }
    } else {
        let mut integrator_params: ParamSet = ParamSet::default();
        integrator_params.add_int(String::from("maxdepth"), args.max_depth as i32);
        let integrator_name: String;
        if render_options.has_emitters {
            integrator_name = String::from("path");
        } else {
            integrator_name = String::from("ao");
        }
        let some_integrator: Option<Box<Integrator>> = make_integrator(
            &integrator_name,
            2.0 as Float,
            render_x as i32,
            render_y as i32,
            fov,
            clipsta,
            animated_cam_to_world,
            args.samples as i32,
            integrator_params,
        );
        if let Some(mut integrator) = some_integrator {
            let scene = make_scene(&render_options.primitives, render_options.lights);
            let num_threads: u8 = num_cpus::get() as u8;
            integrator.render(&scene, num_threads);
        } else {
            panic!("Unable to create integrator.");
        }
    }
    Ok(())
}
