//! Handlers for `tz auth login` and `tz auth logout`.

use owo_colors::OwoColorize;

use super::LoginArgs;
use crate::auth::{Authenticator, LoginMode, TokenSources, print_device_prompt};
use crate::config::ResolvedConfig;
use crate::error::HintedError;

/// Handle `tz auth login`.
pub async fn login(
    config: &ResolvedConfig,
    sources: TokenSources,
    args: &LoginArgs,
) -> Result<(), HintedError> {
    let mode = if args.interactive {
        LoginMode::Interactive
    } else if args.non_interactive {
        LoginMode::NonInteractive
    } else {
        LoginMode::Auto
    };
    let authenticator = Authenticator::new(config, sources)?;
    let claims = authenticator.login(mode, print_device_prompt).await?;
    let who = claims.display_name().unwrap_or("user");
    println!(
        "{} Logged in as {}",
        crate::symbols::OK.green().bold(),
        who.bold()
    );
    Ok(())
}

/// Handle `tz auth logout`.
pub async fn logout(config: &ResolvedConfig, sources: TokenSources) -> Result<(), HintedError> {
    let authenticator = Authenticator::new(config, sources)?;
    let removed = authenticator.logout().map_err(HintedError::new)?;
    if removed {
        println!("{} Logged out", crate::symbols::OK.green().bold());
    } else {
        println!("No cached credentials to remove");
    }
    Ok(())
}
