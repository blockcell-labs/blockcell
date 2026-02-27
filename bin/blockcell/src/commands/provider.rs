use blockcell_core::Config;
use blockcell_providers::Provider;

pub fn create_provider(config: &Config) -> anyhow::Result<Box<dyn Provider>> {
    blockcell_providers::create_main_provider(config)
}

pub fn create_evolution_provider(config: &Config) -> anyhow::Result<Box<dyn Provider>> {
    blockcell_providers::create_evolution_provider(config)
}
