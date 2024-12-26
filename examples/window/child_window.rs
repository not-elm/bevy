//! Uses two windows to visualize a 3D model from different angles.

use bevy::input_focus::InputDispatchPlugin;
use bevy::window::PrimaryWindow;
use bevy::{prelude::*, render::camera::RenderTarget, window::WindowRef};
use bevy_internal::window::WindowResolution;

fn main() {
    App::new()
        // By default, a primary window gets spawned by `WindowPlugin`, contained in `DefaultPlugins`
        .add_plugins((DefaultPlugins, InputDispatchPlugin))
        .add_systems(Startup, spawn_child_window)
        .run();
}

fn spawn_child_window(mut commands: Commands, parent_window: Query<Entity, With<PrimaryWindow>>) {
    commands.entity(parent_window.single()).with_child(Window {
        resolution: WindowResolution::new(300., 300.),
        ..default()
    });
}
