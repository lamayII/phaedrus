//! `CFrame`.

use glam::{EulerRot, Mat3, Quat, Vec3};
use mlua::{FromLua, Lua, MetaMethod, Result, Table, UserData, UserDataFields, UserDataMethods, Value, Variadic};

use super::num;
use super::vector::Vector3;

/// Rotation (columns are Right, Up, Back) plus translation.
#[derive(Debug, Clone, Copy, PartialEq, FromLua)]
pub struct CFrame {
    pub rot: Mat3,
    pub pos: Vec3,
}

impl CFrame {
    pub const IDENTITY: Self = Self { rot: Mat3::IDENTITY, pos: Vec3::ZERO };

    pub fn from_pos(pos: Vec3) -> Self {
        Self { rot: Mat3::IDENTITY, pos }
    }

    pub fn mul(&self, o: &CFrame) -> CFrame {
        CFrame { rot: self.rot * o.rot, pos: self.rot * o.pos + self.pos }
    }

    pub fn inverse(&self) -> CFrame {
        let inv = self.rot.transpose();
        CFrame { rot: inv, pos: -(inv * self.pos) }
    }

    pub fn point_to_world(&self, p: Vec3) -> Vec3 {
        self.rot * p + self.pos
    }

    /// `CFrame.lookAt(at, target, up)`.
    pub fn look_at(at: Vec3, target: Vec3, up: Vec3) -> CFrame {
        let look = (target - at).normalize_or_zero();
        if look == Vec3::ZERO {
            return CFrame::from_pos(at);
        }
        let mut right = look.cross(up);
        if right.length_squared() < 1e-10 {
            right = look.cross(Vec3::Z);
        }
        let right = right.normalize();
        let up = right.cross(look).normalize();
        CFrame { rot: Mat3::from_cols(right, up, -look), pos: at }
    }

    pub fn to_rbx(&self) -> phaedrus_dom::types::CFrame {
        use phaedrus_dom::types::{Matrix3, Vector3 as V};
        let r = self.rot;
        // rbx_types stores rows.
        phaedrus_dom::types::CFrame::new(
            V::new(self.pos.x, self.pos.y, self.pos.z),
            Matrix3::new(
                V::new(r.x_axis.x, r.y_axis.x, r.z_axis.x),
                V::new(r.x_axis.y, r.y_axis.y, r.z_axis.y),
                V::new(r.x_axis.z, r.y_axis.z, r.z_axis.z),
            ),
        )
    }

    pub fn from_rbx(c: &phaedrus_dom::types::CFrame) -> Self {
        let m = &c.orientation;
        Self {
            rot: Mat3::from_cols(
                Vec3::new(m.x.x, m.y.x, m.z.x),
                Vec3::new(m.x.y, m.y.y, m.z.y),
                Vec3::new(m.x.z, m.y.z, m.z.z),
            ),
            pos: Vec3::new(c.position.x, c.position.y, c.position.z),
        }
    }

    /// Roblox component order: x, y, z, R00, R01, R02, R10, ... R22.
    pub fn components(&self) -> [f32; 12] {
        let r = self.rot;
        [
            self.pos.x, self.pos.y, self.pos.z,
            r.x_axis.x, r.y_axis.x, r.z_axis.x,
            r.x_axis.y, r.y_axis.y, r.z_axis.y,
            r.x_axis.z, r.y_axis.z, r.z_axis.z,
        ]
    }

    fn lerp(&self, o: &CFrame, t: f32) -> CFrame {
        let a = Quat::from_mat3(&self.rot);
        let b = Quat::from_mat3(&o.rot);
        CFrame { rot: Mat3::from_quat(a.slerp(b, t)), pos: self.pos.lerp(o.pos, t) }
    }

    /// Euler angles in Roblox's `ToOrientation` convention (YXZ).
    pub fn to_orientation(&self) -> (f32, f32, f32) {
        let (y, x, z) = Quat::from_mat3(&self.rot).to_euler(EulerRot::YXZ);
        (x, y, z)
    }

    pub fn to_euler_xyz(&self) -> (f32, f32, f32) {
        Quat::from_mat3(&self.rot).to_euler(EulerRot::XYZ)
    }
}

/// `CFrame.Angles(rx, ry, rz)`: rotate X, then Y, then Z (applied as Rx*Ry*Rz).
pub fn from_euler_xyz(x: f32, y: f32, z: f32) -> Mat3 {
    Mat3::from_rotation_x(x) * Mat3::from_rotation_y(y) * Mat3::from_rotation_z(z)
}

/// `CFrame.fromOrientation(rx, ry, rz)`: Ry * Rx * Rz.
pub fn from_orientation(x: f32, y: f32, z: f32) -> Mat3 {
    Mat3::from_rotation_y(y) * Mat3::from_rotation_x(x) * Mat3::from_rotation_z(z)
}

impl UserData for CFrame {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_meta_field(MetaMethod::Type, "CFrame");
        f.add_field_method_get("Position", |_, c| Ok(Vector3(c.pos)));
        f.add_field_method_get("p", |_, c| Ok(Vector3(c.pos)));
        f.add_field_method_get("X", |_, c| Ok(c.pos.x));
        f.add_field_method_get("Y", |_, c| Ok(c.pos.y));
        f.add_field_method_get("Z", |_, c| Ok(c.pos.z));
        f.add_field_method_get("Rotation", |_, c| Ok(CFrame { rot: c.rot, pos: Vec3::ZERO }));
        f.add_field_method_get("LookVector", |_, c| Ok(Vector3(-c.rot.z_axis)));
        f.add_field_method_get("RightVector", |_, c| Ok(Vector3(c.rot.x_axis)));
        f.add_field_method_get("UpVector", |_, c| Ok(Vector3(c.rot.y_axis)));
        f.add_field_method_get("XVector", |_, c| Ok(Vector3(c.rot.row(0))));
        f.add_field_method_get("YVector", |_, c| Ok(Vector3(c.rot.row(1))));
        f.add_field_method_get("ZVector", |_, c| Ok(Vector3(c.rot.row(2))));
    }

    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("Inverse", |_, c, ()| Ok(c.inverse()));
        m.add_method("Lerp", |_, c, (o, t): (CFrame, f32)| Ok(c.lerp(&o, t)));
        m.add_method("Orthonormalize", |_, c, ()| {
            Ok(CFrame { rot: Mat3::from_quat(Quat::from_mat3(&c.rot).normalize()), pos: c.pos })
        });
        m.add_method("ToWorldSpace", |_, c, args: Variadic<CFrame>| {
            Ok(Variadic::from_iter(args.iter().map(|o| c.mul(o))))
        });
        m.add_method("ToObjectSpace", |_, c, args: Variadic<CFrame>| {
            let inv = c.inverse();
            Ok(Variadic::from_iter(args.iter().map(|o| inv.mul(o))))
        });
        m.add_method("PointToWorldSpace", |_, c, args: Variadic<Vector3>| {
            Ok(Variadic::from_iter(args.iter().map(|p| Vector3(c.point_to_world(p.0)))))
        });
        m.add_method("PointToObjectSpace", |_, c, args: Variadic<Vector3>| {
            let inv = c.inverse();
            Ok(Variadic::from_iter(args.iter().map(|p| Vector3(inv.point_to_world(p.0)))))
        });
        m.add_method("VectorToWorldSpace", |_, c, args: Variadic<Vector3>| {
            Ok(Variadic::from_iter(args.iter().map(|p| Vector3(c.rot * p.0))))
        });
        m.add_method("VectorToObjectSpace", |_, c, args: Variadic<Vector3>| {
            let inv = c.rot.transpose();
            Ok(Variadic::from_iter(args.iter().map(|p| Vector3(inv * p.0))))
        });
        m.add_method("GetComponents", |_, c, ()| Ok(Variadic::from_iter(c.components())));
        m.add_method("components", |_, c, ()| Ok(Variadic::from_iter(c.components())));
        m.add_method("ToOrientation", |_, c, ()| Ok(c.to_orientation()));
        m.add_method("ToEulerAnglesYXZ", |_, c, ()| Ok(c.to_orientation()));
        m.add_method("ToEulerAnglesXYZ", |_, c, ()| Ok(c.to_euler_xyz()));
        m.add_method("ToAxisAngle", |_, c, ()| {
            let (axis, angle) = Quat::from_mat3(&c.rot).to_axis_angle();
            Ok((Vector3(axis), angle))
        });
        m.add_method("FuzzyEq", |_, c, (o, eps): (CFrame, Option<f32>)| {
            let e = eps.unwrap_or(1e-5);
            Ok(c.pos.abs_diff_eq(o.pos, e) && c.rot.abs_diff_eq(o.rot, e))
        });

        m.add_meta_function(MetaMethod::Mul, |lua, (a, b): (CFrame, Value)| {
            match &b {
                Value::UserData(ud) if ud.is::<CFrame>() => {
                    a.mul(&*ud.borrow::<CFrame>()?).into_lua_value(lua)
                }
                Value::UserData(ud) if ud.is::<Vector3>() => {
                    Vector3(a.point_to_world(ud.borrow::<Vector3>()?.0)).into_lua_value(lua)
                }
                _ => Err(mlua::Error::runtime("CFrame can only be multiplied by CFrame or Vector3")),
            }
        });
        m.add_meta_function(MetaMethod::Add, |_, (a, b): (CFrame, Vector3)| {
            Ok(CFrame { rot: a.rot, pos: a.pos + b.0 })
        });
        m.add_meta_function(MetaMethod::Sub, |_, (a, b): (CFrame, Vector3)| {
            Ok(CFrame { rot: a.rot, pos: a.pos - b.0 })
        });
        m.add_meta_method(MetaMethod::Eq, |_, a, b: CFrame| Ok(a == &b));
        m.add_meta_method(MetaMethod::ToString, |_, c, ()| {
            Ok(c.components().iter().map(|v| super::vector::fmt(*v)).collect::<Vec<_>>().join(", "))
        });
    }
}

trait IntoLuaValue {
    fn into_lua_value(self, lua: &Lua) -> Result<Value>;
}

impl<T: mlua::IntoLua> IntoLuaValue for T {
    fn into_lua_value(self, lua: &Lua) -> Result<Value> {
        self.into_lua(lua)
    }
}

pub fn register(lua: &Lua, g: &Table) -> Result<()> {
    let t = lua.create_table()?;
    t.set(
        "new",
        lua.create_function(|_, args: Variadic<Value>| {
            match args.len() {
                0 => Ok(CFrame::IDENTITY),
                1 => {
                    let p = vec_of(&args[0])?;
                    Ok(CFrame::from_pos(p))
                }
                2 => Ok(CFrame::look_at(vec_of(&args[0])?, vec_of(&args[1])?, Vec3::Y)),
                3 => Ok(CFrame::from_pos(Vec3::new(num(&args[0])?, num(&args[1])?, num(&args[2])?))),
                7 => {
                    // x, y, z, qx, qy, qz, qw
                    let n: Vec<f32> = args.iter().map(num).collect::<Result<_>>()?;
                    let q = Quat::from_xyzw(n[3], n[4], n[5], n[6]).normalize();
                    Ok(CFrame { rot: Mat3::from_quat(q), pos: Vec3::new(n[0], n[1], n[2]) })
                }
                12 => {
                    let n: Vec<f32> = args.iter().map(num).collect::<Result<_>>()?;
                    Ok(CFrame {
                        pos: Vec3::new(n[0], n[1], n[2]),
                        rot: Mat3::from_cols(
                            Vec3::new(n[3], n[6], n[9]),
                            Vec3::new(n[4], n[7], n[10]),
                            Vec3::new(n[5], n[8], n[11]),
                        ),
                    })
                }
                n => Err(mlua::Error::runtime(format!("invalid number of arguments to CFrame.new: {n}"))),
            }
        })?,
    )?;
    let angles = lua.create_function(|_, (x, y, z): (f32, f32, f32)| {
        Ok(CFrame { rot: from_euler_xyz(x, y, z), pos: Vec3::ZERO })
    })?;
    t.set("Angles", angles.clone())?;
    t.set("fromEulerAnglesXYZ", angles)?;
    let orient = lua.create_function(|_, (x, y, z): (f32, f32, f32)| {
        Ok(CFrame { rot: from_orientation(x, y, z), pos: Vec3::ZERO })
    })?;
    t.set("fromOrientation", orient.clone())?;
    t.set("fromEulerAnglesYXZ", orient)?;
    t.set(
        "lookAt",
        lua.create_function(|_, (at, target, up): (Vector3, Vector3, Option<Vector3>)| {
            Ok(CFrame::look_at(at.0, target.0, up.map(|u| u.0).unwrap_or(Vec3::Y)))
        })?,
    )?;
    t.set(
        "fromAxisAngle",
        lua.create_function(|_, (axis, angle): (Vector3, f32)| {
            Ok(CFrame { rot: Mat3::from_axis_angle(axis.0.normalize_or_zero(), angle), pos: Vec3::ZERO })
        })?,
    )?;
    t.set(
        "fromMatrix",
        lua.create_function(|_, (pos, vx, vy, vz): (Vector3, Vector3, Vector3, Option<Vector3>)| {
            let z = vz.map(|v| v.0).unwrap_or_else(|| vx.0.cross(vy.0).normalize());
            Ok(CFrame { rot: Mat3::from_cols(vx.0, vy.0, z), pos: pos.0 })
        })?,
    )?;
    t.set("identity", CFrame::IDENTITY)?;
    g.set("CFrame", t)?;
    Ok(())
}

fn vec_of(v: &Value) -> Result<Vec3> {
    match v {
        Value::UserData(ud) => Ok(ud.borrow::<Vector3>()?.0),
        _ => Err(mlua::Error::runtime("expected Vector3")),
    }
}
