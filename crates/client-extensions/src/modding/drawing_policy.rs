//! Adapts authenticated bundle scope without granting other personal components authority.

use mod_api::drawing::DrawingPermissions;
use server_experience::{manifest::Permission, negotiation::Grant};

/// A remote session always permits bounded plain geometry. Elevated permissions apply only
/// to the explicitly named personal-mod digest and expire with the authenticated negotiation grant.
pub fn remote_permissions(
    grant: Option<&Grant>,
    digest: &[u8; 32],
    now_unix: u64,
) -> DrawingPermissions {
    let plain = DrawingPermissions {
        world_draw: true,
        ..Default::default()
    };
    let Some(grant) = grant.filter(|grant| now_unix < grant.expires_unix) else {
        return plain;
    };
    let digest = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if !grant.offer.offer.scope.personal_mods.contains(&digest) {
        return plain;
    }
    let has = |permission| grant.offer.offer.scope.permissions.contains(&permission);
    DrawingPermissions {
        world_draw: has(Permission::WorldDraw),
        entity_observe: has(Permission::EntityObserve),
        entity_highlight: has(Permission::EntityHighlight),
        render_through_walls: has(Permission::RenderThroughWalls),
        render_material: has(Permission::RenderMaterial),
        render_post: has(Permission::RenderPost),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_negotiation_cannot_elevate_a_personal_mod() {
        let permissions = remote_permissions(None, &[0; 32], 0);
        assert!(permissions.world_draw);
        assert!(
            !permissions.entity_observe
                && !permissions.render_through_walls
                && !permissions.render_post
        );
    }
}
