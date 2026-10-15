//! Test-only crate: compiles the bindings generated from imping-codegen's x64
//! fixture IR, under this workspace's lints, and checks them at run time.
//!
//! Compiling it at all checks the generated layout assertions.

include!(concat!(env!("OUT_DIR"), "/imped.rs"));

/// Text types mapped to the runtime's string types.
#[cfg(test)]
mod strings {
    use super::game;
    use core::mem::{offset_of, size_of};
    use imping_runtime::{CStrBuf, CStrPtr, WStrBuf, WStrPtr, WString};

    unsafe extern "system" fn set_title_like(_: WStrPtr) {}

    unsafe extern "system" fn rename_like(_: *mut game::Named, _: CStrPtr, _: *mut u8) -> CStrPtr {
        CStrPtr::null()
    }

    #[test]
    fn struct_fields_use_the_string_types() {
        let mut named = game::Named {
            name: CStrPtr::from(c"Hero"),
            ..game::Named::default()
        };
        named.tag.set("[Guild Of Many]");
        named.title.set("Sir");
        // SAFETY: `name` points to a static string.
        assert_eq!(unsafe { named.name.to_string_lossy() }, "Hero");
        assert_eq!(named.tag.to_string_lossy(), "[Guild ");
        assert_eq!(named.title.to_string_lossy(), "Sir");

        let _: CStrBuf<8> = named.tag;
        let _: WStrBuf<4> = named.title;
        assert_eq!(size_of::<game::Named>(), 24);
        assert_eq!(offset_of!(game::Named, title), 16);
        assert!(game::Entity::default().name.is_null());
    }

    #[test]
    fn signatures_use_the_string_pointers() {
        let _: game::ui::SetTitle::Signature = set_title_like;
        let _: game::ui::Rename::Signature = rename_like;
        let _: game::LPCSTR = CStrPtr::null();
        let title = WString::from("Sir");
        // SAFETY: The function ignores its argument.
        unsafe { set_title_like(title.as_ptr()) };
    }
}

/// Generated functions and globals. `game.exe` is not loaded in the test
/// process, so these check everything up to `init`; calling and hooking real
/// functions through generated code is covered end to end with a fixture game
/// later, and by imping-runtime's own tests of the same plumbing.
#[cfg(test)]
mod functions {
    use super::game;
    use imping_runtime::Error;

    unsafe extern "system" fn update_like(_: *mut game::Entity, _: f32) {}

    #[test]
    fn init_fails_when_the_module_is_not_loaded() {
        // SAFETY: `game.exe` is not loaded, so nothing is done through the
        // bindings.
        let error = unsafe { game::init() }.unwrap_err();
        assert!(matches!(
            error,
            Error::ModuleNotLoaded { module: "game.exe" }
        ));
        assert_eq!(game::MODULE.name(), "game.exe");
        assert_eq!(game::MODULE.base(), None);
    }

    #[test]
    fn hooks_fail_before_init() {
        let error = game::entity::Update::hook(|original, _, _| original.forward()).unwrap_err();
        assert!(matches!(
            error,
            Error::NotInitialized { module: "game.exe" }
        ));
        let error = game::entity::Update::hook(|original, this, dt| {
            // SAFETY: Never runs; the hook is not installed.
            unsafe { original.call(this, dt * 0.5) }
        })
        .unwrap_err();
        assert!(matches!(error, Error::NotInitialized { .. }));
    }

    #[test]
    #[should_panic(expected = "imping: `game.exe` is not initialized; call its `init()` first")]
    fn function_pointers_panic_before_init() {
        game::GetPlayer::ptr();
    }

    #[test]
    #[should_panic(expected = "is not initialized")]
    fn unsafe_calls_panic_before_init() {
        // SAFETY: It panics before calling anything, since `init` hasn't run.
        unsafe { game::GetPlayer() };
    }

    #[test]
    #[should_panic(expected = "is not initialized")]
    fn safe_calls_panic_before_init() {
        game::math::Normalize(game::Vector3::default());
    }

    #[test]
    #[should_panic(expected = "is not initialized")]
    fn globals_panic_before_init() {
        game::gGravity.read();
    }

    #[test]
    fn functions_tagged_imping_safe_have_safe_wrappers() {
        let _: fn(&mut game::Entity, i32) = game::entity::Damage;
        let _: fn(game::Vector3) -> game::Vector3 = game::math::Normalize;
        let _: fn(&imping_runtime::WString) = game::ui::SetTitle;
        let _: fn(&mut game::Named, &core::ffi::CStr, &mut u8) -> imping_runtime::CStrPtr =
            game::ui::Rename;
        // Tagged, but a parameter has no safe form: still `unsafe`.
        let _: unsafe fn(*mut core::ffi::c_void) = game::util::Release;
        let _: unsafe fn(game::Holder) = game::util::Store;
        // Untagged overload.
        let _: unsafe fn(*mut game::Entity, f32) = game::entity::Damage_2;
    }

    #[test]
    fn plain_types_and_globals() {
        fn assert_plain<T: imping_runtime::Plain>() {}
        assert_plain::<game::Vector3>();
        assert_plain::<game::EntityKind>();
        assert_plain::<game::Named>();
        assert_plain::<game::Value>();
        assert_plain::<game::Broken>();
        assert_plain::<game::Holder>();
        let _: fn(&imping_runtime::Global<f32>) -> f32 = imping_runtime::Global::read;
        let _: fn(&imping_runtime::Global<*mut game::Entity>, *mut game::Entity) =
            imping_runtime::Global::write;
        let _ = &game::gPlayer;
    }

    #[test]
    fn addresses_names_and_signatures() {
        assert_eq!(game::GetPlayer::RVA, 0x15200);
        assert_eq!(game::entity::Update::RVA, 0x15040);
        assert_eq!(game::entity::Update::VERIFICATION, [0x48, 0x83, 0xec, 0x28]);
        assert_eq!(
            game::entity::Damage::RVA,
            0x15300,
            "the overload with the lowest RVA keeps the plain name"
        );
        assert_eq!(game::entity::Damage_2::RVA, 0x15400);
        assert_eq!(game::entity::Entity::RVA, 0x15000, "a constructor");
        assert_eq!(game::init_2::RVA, 0x15d00, "`init` is reserved");
        assert_eq!(game::util::FastPath::RVA, 0x15b00);
        assert_eq!(game::util::Log::RVA, 0x15a00);
        assert_eq!(game::game::world::Tick::RVA, 0x16000);
        assert_eq!(game::gGravity.rva(), 0x3a1200);
        assert_eq!(game::gPlayer.rva(), 0x3a1400);
        assert_eq!(game::entity::sCount.rva(), 0x3a1300);
        assert_eq!(game::entity::sCount.module().name(), "game.exe");

        let _: game::entity::Update::Signature = update_like;
        let _: fn() -> game::util::Mixed::Signature = game::util::Mixed::ptr;
        let _: Option<game::util::Log::Signature> = None;
        let _: unsafe fn(*mut game::Entity) -> *mut game::Entity = game::entity::Entity;
    }
}

#[cfg(test)]
mod tests {
    use super::game::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn struct_defaults_are_zero() {
        let entity = Entity::default();
        assert!(entity.vtable.is_null());
        assert_eq!(entity.kind, EntityKind::None);
        assert_eq!(entity._bitfield_0xc, 0);
        assert_eq!(entity.health, 0.0);
        assert!(entity.target.is_null());
        assert!(entity.on_hit.is_none());
        assert_eq!(Matrix::default().rows, [[0.0; 4]; 4]);
    }

    #[test]
    fn unsigned_and_bool_bitfields_round_trip_without_touching_neighbours() {
        let mut entity = Entity::default();
        entity.set_visible(true);
        entity.set_team(5);
        assert!(entity.visible());
        assert_eq!(entity.team(), 5);
        assert_eq!(entity._bitfield_0xc, 0b1011);

        entity.set_team(0xff);
        assert_eq!(entity.team(), 7, "bits beyond the width are discarded");
        assert!(entity.visible());
        entity.set_visible(false);
        assert_eq!(entity.team(), 7);
        assert_eq!(entity._bitfield_0xc, 0b1110);
    }

    #[test]
    fn signed_bitfields_sign_extend() {
        let mut entity = Entity::default();
        for value in -16..=15 {
            entity.set_level(value);
            assert_eq!(entity.level(), value);
        }
        entity.set_level(-1);
        assert_eq!(entity._bitfield_0xc, 0b1_1111 << 4);
        entity.set_team(3);
        assert_eq!(entity.level(), -1);
    }

    #[test]
    fn enum_bitfields_wrap_the_newtype() {
        let mut entity = Entity::default();
        entity.set_state(EntityKind::Npc);
        assert_eq!(entity.state(), EntityKind::Npc);
        assert_eq!(entity._bitfield_0xc, 2 << 9);
    }

    #[test]
    fn wide_bitfields_use_64_bit_storage() {
        let mut wide = Wide::default();
        wide.set_lo(0xff_ffff_ffff);
        wide.set_hi(-2);
        assert_eq!(wide.lo(), 0xff_ffff_ffff);
        assert_eq!(wide.hi(), -2);
        wide.set_hi(0x7ffff);
        assert_eq!(wide.hi(), 0x7ffff);
        assert_eq!(wide.lo(), 0xff_ffff_ffff);
    }

    #[test]
    fn enum_constants_match_and_debug_by_name() {
        let kind = EntityKind::Player;
        let label = match kind {
            EntityKind::None => "none",
            EntityKind::Player => "player",
            _ => "other",
        };
        assert_eq!(label, "player");
        assert_eq!(format!("{:?}", EntityKind::Npc), "Npc");
        assert_eq!(
            format!("{:?}", EntityKind::Default),
            "None",
            "aliases print the first name"
        );
        assert_eq!(format!("{:?}", EntityKind(7)), "EntityKind(7)");
        assert_eq!(EntityKind::Invalid.0, u32::MAX);
        assert_eq!(SmallSigned::Neg.0, -1);
        assert_eq!(SmallSigned::Big.0, 200u8 as i8);
    }

    #[test]
    fn layouts_match_the_ir() {
        assert_eq!(
            (size_of::<PackedHeader>(), align_of::<PackedHeader>()),
            (7, 1)
        );
        assert_eq!(offset_of!(PackedHeader, length), 1);
        assert_eq!((size_of::<Aligned16>(), align_of::<Aligned16>()), (16, 16));
        assert_eq!((size_of::<Value>(), align_of::<Value>()), (8, 8));
        assert_eq!((size_of::<Small>(), align_of::<Small>()), (4, 4));
        assert_eq!((size_of::<Broken>(), align_of::<Broken>()), (8, 4));
        assert_eq!(offset_of!(UsesBroken, tail), 8);
        assert_eq!(size_of::<Renderer>(), 0);
        assert_eq!(size_of::<Callback>(), 8);
    }

    #[test]
    fn packed_fields_are_readable_by_value() {
        let header = PackedHeader {
            magic: 0x7f,
            length: 0x1234_5678,
            flags: 3,
        };
        let length = header.length;
        assert_eq!(length, 0x1234_5678);
        assert_eq!(
            format!("{header:?}"),
            "PackedHeader { magic: 127, length: 305419896, flags: 3 }"
        );
    }

    #[test]
    fn unions_and_keywords() {
        let value = Value { f: 1.0 };
        // SAFETY: `f` was the field written, and `i` shares its 4 bytes.
        assert_eq!(unsafe { value.i }, 1.0f32.to_bits() as i32);
        assert_eq!(format!("{value:?}"), "Value { .. }");

        let keywords = Keywords {
            r#type: 1,
            self_: 2,
            ..Keywords::default()
        };
        assert_eq!((keywords.r#type, keywords.self_), (1, 2));
        let _renamed: init_39 = init_39 { v: 1 };
        let _duplicate: Vector3_30 = Vector3_30 { x: 1.0, y: 2.0 };
        let _alias: DWORD = 5u32;
    }

    #[test]
    fn function_pointers_are_nullable_and_callable() {
        unsafe extern "system" fn on_hit(entity: *mut Entity, damage: f32) {
            // SAFETY: the test passes a valid, exclusive entity.
            unsafe { (*entity).health -= damage };
        }
        let mut entity = Entity {
            health: 10.0,
            on_hit: Some(on_hit),
            ..Entity::default()
        };
        let callback: Callback = entity.on_hit;
        // SAFETY: `on_hit` has this signature and `entity` is valid.
        unsafe { callback.expect("set above")(&raw mut entity, 4.0) };
        assert_eq!(entity.health, 6.0);
    }
}
