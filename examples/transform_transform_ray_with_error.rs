extern crate pbrt;

use pbrt::{Point3f, Ray, Transform, Vector3f};

fn main() {
    let t: Transform = Transform::translate(Vector3f {
        x: -1.3,
        y: 0.0,
        z: 0.0,
    });
    let o: Point3f = Point3f {
        x: 2.0,
        y: 1.99999988,
        z: 4.99999905,
    };
    let d: Vector3f = Vector3f {
        x: -0.0607556403,
        y: -0.164096087,
        z: -0.984571517,
    };
    let r: Ray = Ray { o: o, d: d };
    let mut o_error: Vector3f = Vector3f::default();
    let mut d_error: Vector3f = Vector3f::default();
    let tr: Ray = t.transform_ray_with_error(r, &mut o_error, &mut d_error);

    println!("t = {:?}", t);
    println!("r = {:?}", r);
    println!("tp = transform_point_with_error(r, {:?}, {:?}) = {:?}",
             o_error,
             d_error,
             tr);
}