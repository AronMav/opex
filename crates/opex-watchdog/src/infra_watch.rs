//! Pure-классификатор состояния docker-контейнеров для self-healing.
//! Логика детекции отделена от IO ради тестируемости (образец — infra_jobs.rs).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerClass {
    Healthy,
    Transient,
    Problem,
}

pub fn classify(status: &str) -> ContainerClass {
    if status.starts_with("Up") {
        return ContainerClass::Healthy;
    }
    // `docker ps -a` статусы: "Created", "Restarting (1) 3s ago",
    // "Exited (0) 2 min ago", "Dead".
    let s = status.trim_start();
    if s.starts_with("Created")
        || s.starts_with("Restarting")
        || s.starts_with("Dead")
        || s.starts_with("Exited")
    {
        return ContainerClass::Problem;
    }
    ContainerClass::Transient
}

pub fn should_trigger(class: ContainerClass, streak: u32, grace: u32) -> bool {
    class == ContainerClass::Problem && streak >= grace
}

pub fn is_excluded(docker_name: &str) -> bool {
    docker_name.contains("postgres") || docker_name.starts_with("mcp-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn up_is_healthy() {
        assert_eq!(classify("Up 3 hours"), ContainerClass::Healthy);
        assert_eq!(classify("Up 2 minutes (healthy)"), ContainerClass::Healthy);
    }

    #[test]
    fn created_and_exited_are_problem() {
        assert_eq!(classify("Created"), ContainerClass::Problem);
        assert_eq!(classify("Exited (0) 5 minutes ago"), ContainerClass::Problem);
        assert_eq!(classify("Restarting (1) 2s ago"), ContainerClass::Problem);
        assert_eq!(classify("Dead"), ContainerClass::Problem);
    }

    #[test]
    fn trigger_only_after_grace() {
        assert!(!should_trigger(ContainerClass::Problem, 1, 2));
        assert!(should_trigger(ContainerClass::Problem, 2, 2));
        assert!(!should_trigger(ContainerClass::Healthy, 5, 2));
    }

    #[test]
    fn postgres_and_mcp_excluded() {
        assert!(is_excluded("docker-postgres-1"));
        assert!(is_excluded("mcp-github"));
        assert!(!is_excluded("docker-tts-silero-1"));
    }
}
