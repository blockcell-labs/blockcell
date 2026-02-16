use async_trait::async_trait;
use blockcell_core::{Error, Result};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::debug;

use crate::{Tool, ToolContext, ToolSchema};

/// Unified cloud platform management tool.
///
/// Supported providers:
/// - **AWS**: EC2 instances, CloudWatch metrics, Cost Explorer, S3 operations, ECS services
/// - **GCP**: Compute Engine, Cloud Monitoring, Billing, GCS, Cloud Run
/// - **Azure**: VMs, Monitor metrics, Cost Management, Blob Storage, Container Instances
///
/// Uses REST APIs with bearer/OAuth tokens when available, falls back to CLI tools
/// (aws, gcloud, az) when tokens are not provided.
pub struct CloudApiTool;

#[async_trait]
impl Tool for CloudApiTool {
    fn schema(&self) -> ToolSchema {
        let str_prop = |desc: &str| -> Value { json!({"type": "string", "description": desc}) };
        let obj_prop = |desc: &str| -> Value { json!({"type": "object", "description": desc}) };
        let int_prop = |desc: &str| -> Value { json!({"type": "integer", "description": desc}) };
        let bool_prop = |desc: &str| -> Value { json!({"type": "boolean", "description": desc}) };

        let mut props = serde_json::Map::new();
        props.insert("provider".into(), json!({"type": "string", "enum": ["aws", "gcp", "azure"], "description": "Cloud provider"}));
        props.insert("action".into(), str_prop("Action to perform. AWS: list_instances|get_instance|start_instance|stop_instance|get_metrics|get_costs|list_buckets|list_objects|list_services. GCP: list_instances|get_instance|start_instance|stop_instance|get_metrics|get_costs|list_buckets|list_objects|list_services. Azure: list_vms|get_vm|start_vm|stop_vm|get_metrics|get_costs|list_containers|list_blobs|list_services."));
        props.insert("region".into(), str_prop("Cloud region (e.g. 'us-east-1', 'us-central1', 'eastus')"));
        props.insert("instance_id".into(), str_prop("Instance/VM ID for get/start/stop operations"));
        props.insert("service_name".into(), str_prop("Service/cluster name for ECS/Cloud Run/Container Instances"));
        props.insert("bucket".into(), str_prop("S3/GCS/Blob storage bucket name"));
        props.insert("prefix".into(), str_prop("Object key prefix for listing objects"));
        props.insert("metric_name".into(), str_prop("CloudWatch/Monitoring metric name (e.g. 'CPUUtilization', 'NetworkIn')"));
        props.insert("metric_namespace".into(), str_prop("(AWS) CloudWatch namespace (e.g. 'AWS/EC2', 'AWS/ECS')"));
        props.insert("start_time".into(), str_prop("Start time for metrics/costs query (ISO 8601)"));
        props.insert("end_time".into(), str_prop("End time for metrics/costs query (ISO 8601)"));
        props.insert("period".into(), int_prop("(metrics) Aggregation period in seconds (default: 300)"));
        props.insert("granularity".into(), str_prop("(costs) Time granularity: 'DAILY' or 'MONTHLY'"));
        props.insert("group_by".into(), str_prop("(costs) Group by dimension: 'SERVICE', 'REGION', 'INSTANCE_TYPE'"));
        props.insert("project_id".into(), str_prop("(GCP) Project ID"));
        props.insert("subscription_id".into(), str_prop("(Azure) Subscription ID"));
        props.insert("resource_group".into(), str_prop("(Azure) Resource group name"));
        props.insert("api_key".into(), str_prop("API key or access token (overrides config/env)"));
        props.insert("api_secret".into(), str_prop("API secret key (AWS secret, Azure client secret)"));
        props.insert("max_results".into(), int_prop("Maximum number of results (default: 50)"));
        props.insert("filters".into(), obj_prop("Additional filters as key-value pairs"));
        props.insert("dry_run".into(), bool_prop("If true, validate the request without executing (for start/stop)"));

        ToolSchema {
            name: "cloud_api",
            description: "Manage cloud infrastructure across AWS, GCP, and Azure. List/start/stop instances, query metrics (CPU, memory, network), check costs, manage storage buckets, and monitor container services. Requires cloud credentials via parameters, config providers section, or environment variables (AWS_ACCESS_KEY_ID, GCP_ACCESS_TOKEN, AZURE_ACCESS_TOKEN). Falls back to CLI tools (aws/gcloud/az) when installed.",
            parameters: json!({
                "type": "object",
                "properties": Value::Object(props),
                "required": ["provider", "action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let provider = params.get("provider").and_then(|v| v.as_str()).unwrap_or("");
        if !["aws", "gcp", "azure"].contains(&provider) {
            return Err(Error::Tool("provider must be 'aws', 'gcp', or 'azure'".into()));
        }
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        if action.is_empty() {
            return Err(Error::Tool("action is required".into()));
        }
        let valid_actions: &[&str] = match provider {
            "aws" => &["list_instances", "get_instance", "start_instance", "stop_instance", "get_metrics", "get_costs", "list_buckets", "list_objects", "list_services"],
            "gcp" => &["list_instances", "get_instance", "start_instance", "stop_instance", "get_metrics", "get_costs", "list_buckets", "list_objects", "list_services"],
            "azure" => &["list_vms", "get_vm", "start_vm", "stop_vm", "get_metrics", "get_costs", "list_containers", "list_blobs", "list_services"],
            _ => &[],
        };
        if !valid_actions.contains(&action) {
            return Err(Error::Tool(format!("Invalid action '{}' for provider '{}'", action, provider)));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let provider = params["provider"].as_str().unwrap_or("aws");
        let action = params["action"].as_str().unwrap_or("");
        match provider {
            "aws" => self.execute_aws(&ctx, &params, action).await,
            "gcp" => self.execute_gcp(&ctx, &params, action).await,
            "azure" => self.execute_azure(&ctx, &params, action).await,
            _ => Err(Error::Tool(format!("Unknown provider: {}", provider))),
        }
    }
}

impl CloudApiTool {
    // ─── Credential helpers ───

    fn resolve_gcp_token(ctx: &ToolContext, params: &Value) -> String {
        params.get("api_key").and_then(|v| v.as_str()).map(String::from)
            .or_else(|| ctx.config.providers.get("gcp").map(|p| p.api_key.clone()))
            .or_else(|| std::env::var("GCP_ACCESS_TOKEN").ok())
            .unwrap_or_default()
    }

    fn resolve_azure_token(ctx: &ToolContext, params: &Value) -> String {
        params.get("api_key").and_then(|v| v.as_str()).map(String::from)
            .or_else(|| ctx.config.providers.get("azure").map(|p| p.api_key.clone()))
            .or_else(|| std::env::var("AZURE_ACCESS_TOKEN").ok())
            .unwrap_or_default()
    }

    // ─── Generic helpers ───

    async fn rest_get(client: &Client, url: &str, token: &str) -> Result<Value> {
        let resp = client.get(url)
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
            .map_err(|e| Error::Tool(format!("API request failed: {}", e)))?;
        let status = resp.status();
        let body = resp.text().await
            .map_err(|e| Error::Tool(format!("Failed to read response: {}", e)))?;
        if !status.is_success() {
            return Err(Error::Tool(format!("API error ({}): {}", status, truncate_body(&body, 500))));
        }
        Ok(serde_json::from_str(&body).unwrap_or_else(|_| json!({"output": truncate_body(&body, 4000)})))
    }

    async fn rest_post(client: &Client, url: &str, token: &str, payload: Option<&Value>) -> Result<Value> {
        let mut req = client.post(url)
            .header("Authorization", format!("Bearer {}", token));
        if let Some(body) = payload {
            req = req.json(body);
        } else {
            req = req.header("Content-Length", "0");
        }
        let resp = req.send().await
            .map_err(|e| Error::Tool(format!("API request failed: {}", e)))?;
        let status = resp.status();
        let body = resp.text().await
            .map_err(|e| Error::Tool(format!("Failed to read response: {}", e)))?;
        if !status.is_success() {
            return Err(Error::Tool(format!("API error ({}): {}", status, truncate_body(&body, 500))));
        }
        if body.is_empty() {
            Ok(json!({"status": "accepted", "code": status.as_u16()}))
        } else {
            Ok(serde_json::from_str(&body).unwrap_or_else(|_| json!({"output": truncate_body(&body, 4000)})))
        }
    }

    async fn run_cli(cmd: &str) -> Result<Value> {
        debug!(cmd = %cmd, "CLI fallback");
        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .output()
            .await
            .map_err(|e| Error::Tool(format!("Failed to run CLI command: {}", e)))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            return Err(Error::Tool(format!("CLI error: {}{}", truncate_body(&stdout, 2000), truncate_body(&stderr, 2000))));
        }
        match serde_json::from_str::<Value>(&stdout) {
            Ok(v) => Ok(v),
            Err(_) => Ok(json!({"output": truncate_body(&stdout, 4000)})),
        }
    }

    // ─── AWS (CLI-based, most reliable without SDK) ───

    async fn execute_aws(&self, _ctx: &ToolContext, params: &Value, action: &str) -> Result<Value> {
        let region = params.get("region").and_then(|v| v.as_str()).unwrap_or("us-east-1");
        let cmd = match action {
            "list_instances" => {
                format!("aws ec2 describe-instances --region {} --output json 2>&1", region)
            }
            "get_instance" => {
                let id = params.get("instance_id").and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Tool("instance_id is required".into()))?;
                format!("aws ec2 describe-instances --instance-ids {} --region {} --output json 2>&1", id, region)
            }
            "start_instance" => {
                let id = params.get("instance_id").and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Tool("instance_id is required".into()))?;
                let dry = if params.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(false) { " --dry-run" } else { "" };
                format!("aws ec2 start-instances --instance-ids {}{} --region {} --output json 2>&1", id, dry, region)
            }
            "stop_instance" => {
                let id = params.get("instance_id").and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Tool("instance_id is required".into()))?;
                let dry = if params.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(false) { " --dry-run" } else { "" };
                format!("aws ec2 stop-instances --instance-ids {}{} --region {} --output json 2>&1", id, dry, region)
            }
            "get_metrics" => {
                let metric = params.get("metric_name").and_then(|v| v.as_str()).unwrap_or("CPUUtilization");
                let ns = params.get("metric_namespace").and_then(|v| v.as_str()).unwrap_or("AWS/EC2");
                let period = params.get("period").and_then(|v| v.as_u64()).unwrap_or(300);
                let start = params.get("start_time").and_then(|v| v.as_str()).map(|s| s.to_string())
                    .unwrap_or_else(|| (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339());
                let end = params.get("end_time").and_then(|v| v.as_str()).map(|s| s.to_string())
                    .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
                format!(
                    "aws cloudwatch get-metric-statistics --namespace '{}' --metric-name {} --period {} --statistics Average --start-time {} --end-time {} --region {} --output json 2>&1",
                    ns, metric, period, start, end, region
                )
            }
            "get_costs" => {
                let gran = params.get("granularity").and_then(|v| v.as_str()).unwrap_or("DAILY");
                let gb = params.get("group_by").and_then(|v| v.as_str()).unwrap_or("SERVICE");
                let start = params.get("start_time").and_then(|v| v.as_str()).map(|s| s.to_string())
                    .unwrap_or_else(|| (chrono::Utc::now() - chrono::Duration::days(30)).format("%Y-%m-%d").to_string());
                let end = params.get("end_time").and_then(|v| v.as_str()).map(|s| s.to_string())
                    .unwrap_or_else(|| chrono::Utc::now().format("%Y-%m-%d").to_string());
                format!(
                    "aws ce get-cost-and-usage --granularity {} --metrics BlendedCost --time-period Start={},End={} --group-by Type=DIMENSION,Key={} --output json 2>&1",
                    gran, start, end, gb
                )
            }
            "list_buckets" => "aws s3api list-buckets --output json 2>&1".to_string(),
            "list_objects" => {
                let bucket = params.get("bucket").and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Tool("bucket is required".into()))?;
                let prefix = params.get("prefix").and_then(|v| v.as_str()).unwrap_or("");
                let max = params.get("max_results").and_then(|v| v.as_u64()).unwrap_or(50);
                if prefix.is_empty() {
                    format!("aws s3api list-objects-v2 --bucket {} --max-items {} --output json 2>&1", bucket, max)
                } else {
                    format!("aws s3api list-objects-v2 --bucket {} --prefix '{}' --max-items {} --output json 2>&1", bucket, prefix, max)
                }
            }
            "list_services" => {
                format!("aws ecs list-services --region {} --output json 2>&1", region)
            }
            _ => return Err(Error::Tool(format!("Unknown AWS action: {}", action))),
        };
        Self::run_cli(&cmd).await
    }

    // ─── GCP ───

    async fn execute_gcp(&self, ctx: &ToolContext, params: &Value, action: &str) -> Result<Value> {
        let token = Self::resolve_gcp_token(ctx, params);
        let project_id = params.get("project_id").and_then(|v| v.as_str()).map(String::from)
            .or_else(|| std::env::var("GCP_PROJECT_ID").ok())
            .or_else(|| std::env::var("GOOGLE_CLOUD_PROJECT").ok())
            .unwrap_or_default();
        let region = params.get("region").and_then(|v| v.as_str()).unwrap_or("us-central1");

        // If we have a token, use REST API
        if !token.is_empty() && !project_id.is_empty() {
            let client = Client::new();
            return match action {
                "list_instances" => {
                    let zone = Self::gcp_zone(region);
                    let url = format!("https://compute.googleapis.com/compute/v1/projects/{}/zones/{}/instances", project_id, zone);
                    Self::rest_get(&client, &url, &token).await
                }
                "get_instance" => {
                    let id = params.get("instance_id").and_then(|v| v.as_str())
                        .ok_or_else(|| Error::Tool("instance_id is required".into()))?;
                    let zone = Self::gcp_zone(region);
                    let url = format!("https://compute.googleapis.com/compute/v1/projects/{}/zones/{}/instances/{}", project_id, zone, id);
                    Self::rest_get(&client, &url, &token).await
                }
                "start_instance" | "stop_instance" => {
                    let id = params.get("instance_id").and_then(|v| v.as_str())
                        .ok_or_else(|| Error::Tool("instance_id is required".into()))?;
                    let zone = Self::gcp_zone(region);
                    let op = if action == "start_instance" { "start" } else { "stop" };
                    let url = format!("https://compute.googleapis.com/compute/v1/projects/{}/zones/{}/instances/{}/{}", project_id, zone, id, op);
                    Self::rest_post(&client, &url, &token, Some(&json!({}))).await
                }
                "get_metrics" => {
                    let metric = params.get("metric_name").and_then(|v| v.as_str()).unwrap_or("compute.googleapis.com/instance/cpu/utilization");
                    let start = params.get("start_time").and_then(|v| v.as_str()).map(|s| s.to_string())
                        .unwrap_or_else(|| (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339());
                    let end = params.get("end_time").and_then(|v| v.as_str()).map(|s| s.to_string())
                        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
                    let url = format!(
                        "https://monitoring.googleapis.com/v3/projects/{}/timeSeries?filter=metric.type%3D%22{}%22&interval.startTime={}&interval.endTime={}",
                        project_id, urlencoding::encode(metric), urlencoding::encode(&start), urlencoding::encode(&end)
                    );
                    Self::rest_get(&client, &url, &token).await
                }
                "list_buckets" => {
                    let url = format!("https://storage.googleapis.com/storage/v1/b?project={}", project_id);
                    Self::rest_get(&client, &url, &token).await
                }
                "list_objects" => {
                    let bucket = params.get("bucket").and_then(|v| v.as_str())
                        .ok_or_else(|| Error::Tool("bucket is required".into()))?;
                    let prefix = params.get("prefix").and_then(|v| v.as_str()).unwrap_or("");
                    let max = params.get("max_results").and_then(|v| v.as_u64()).unwrap_or(50);
                    let mut url = format!("https://storage.googleapis.com/storage/v1/b/{}/o?maxResults={}", bucket, max);
                    if !prefix.is_empty() {
                        url.push_str(&format!("&prefix={}", urlencoding::encode(prefix)));
                    }
                    Self::rest_get(&client, &url, &token).await
                }
                "list_services" => {
                    let url = format!("https://run.googleapis.com/v2/projects/{}/locations/{}/services", project_id, region);
                    Self::rest_get(&client, &url, &token).await
                }
                _ => self.gcp_cli_fallback(params, action, &project_id).await,
            };
        }

        // Fallback to gcloud CLI
        self.gcp_cli_fallback(params, action, &project_id).await
    }

    fn gcp_zone(region: &str) -> String {
        if region.chars().last().map(|c| c.is_alphabetic()).unwrap_or(false) && region.matches('-').count() >= 2 {
            region.to_string()
        } else {
            format!("{}-a", region)
        }
    }

    async fn gcp_cli_fallback(&self, params: &Value, action: &str, project_id: &str) -> Result<Value> {
        let proj = if project_id.is_empty() { String::new() } else { format!(" --project {}", project_id) };
        let cmd = match action {
            "list_instances" => format!("gcloud compute instances list{} --format json 2>&1", proj),
            "get_instance" => {
                let id = params.get("instance_id").and_then(|v| v.as_str()).unwrap_or("");
                let zone = params.get("region").and_then(|v| v.as_str()).unwrap_or("us-central1-a");
                format!("gcloud compute instances describe {} --zone {}{} --format json 2>&1", id, zone, proj)
            }
            "start_instance" => {
                let id = params.get("instance_id").and_then(|v| v.as_str()).unwrap_or("");
                let zone = params.get("region").and_then(|v| v.as_str()).unwrap_or("us-central1-a");
                format!("gcloud compute instances start {} --zone {}{} --format json 2>&1", id, zone, proj)
            }
            "stop_instance" => {
                let id = params.get("instance_id").and_then(|v| v.as_str()).unwrap_or("");
                let zone = params.get("region").and_then(|v| v.as_str()).unwrap_or("us-central1-a");
                format!("gcloud compute instances stop {} --zone {}{} --format json 2>&1", id, zone, proj)
            }
            "get_metrics" => {
                let metric = params.get("metric_name").and_then(|v| v.as_str()).unwrap_or("compute.googleapis.com/instance/cpu/utilization");
                format!("gcloud monitoring time-series list --filter='metric.type=\"{}\"'{} --format json 2>&1", metric, proj)
            }
            "get_costs" => "gcloud billing accounts list --format json 2>&1".to_string(),
            "list_buckets" => format!("gcloud storage buckets list{} --format json 2>&1", proj),
            "list_objects" => {
                let bucket = params.get("bucket").and_then(|v| v.as_str()).unwrap_or("");
                format!("gcloud storage objects list gs://{} --format json 2>&1", bucket)
            }
            "list_services" => {
                let region = params.get("region").and_then(|v| v.as_str()).unwrap_or("us-central1");
                format!("gcloud run services list --region {}{} --format json 2>&1", region, proj)
            }
            _ => return Err(Error::Tool(format!("Unknown GCP action: {}", action))),
        };
        Self::run_cli(&cmd).await
    }

    // ─── Azure ───

    async fn execute_azure(&self, ctx: &ToolContext, params: &Value, action: &str) -> Result<Value> {
        let token = Self::resolve_azure_token(ctx, params);
        let sub_id = params.get("subscription_id").and_then(|v| v.as_str()).map(String::from)
            .or_else(|| std::env::var("AZURE_SUBSCRIPTION_ID").ok())
            .unwrap_or_default();
        let rg = params.get("resource_group").and_then(|v| v.as_str()).unwrap_or("");

        if !token.is_empty() && !sub_id.is_empty() {
            let client = Client::new();
            let api_ver = "2023-09-01";
            return match action {
                "list_vms" => {
                    let url = if rg.is_empty() {
                        format!("https://management.azure.com/subscriptions/{}/providers/Microsoft.Compute/virtualMachines?api-version={}", sub_id, api_ver)
                    } else {
                        format!("https://management.azure.com/subscriptions/{}/resourceGroups/{}/providers/Microsoft.Compute/virtualMachines?api-version={}", sub_id, rg, api_ver)
                    };
                    Self::rest_get(&client, &url, &token).await
                }
                "get_vm" => {
                    let name = params.get("instance_id").and_then(|v| v.as_str())
                        .ok_or_else(|| Error::Tool("instance_id (VM name) is required".into()))?;
                    if rg.is_empty() { return Err(Error::Tool("resource_group is required for get_vm".into())); }
                    let url = format!("https://management.azure.com/subscriptions/{}/resourceGroups/{}/providers/Microsoft.Compute/virtualMachines/{}?api-version={}&$expand=instanceView", sub_id, rg, name, api_ver);
                    Self::rest_get(&client, &url, &token).await
                }
                "start_vm" | "stop_vm" => {
                    let name = params.get("instance_id").and_then(|v| v.as_str())
                        .ok_or_else(|| Error::Tool("instance_id (VM name) is required".into()))?;
                    if rg.is_empty() { return Err(Error::Tool("resource_group is required".into())); }
                    let op = if action == "start_vm" { "start" } else { "deallocate" };
                    let url = format!("https://management.azure.com/subscriptions/{}/resourceGroups/{}/providers/Microsoft.Compute/virtualMachines/{}/{}?api-version={}", sub_id, rg, name, op, api_ver);
                    Self::rest_post(&client, &url, &token, None).await
                }
                "get_metrics" => {
                    let metric = params.get("metric_name").and_then(|v| v.as_str()).unwrap_or("Percentage CPU");
                    let name = params.get("instance_id").and_then(|v| v.as_str()).unwrap_or("");
                    if rg.is_empty() || name.is_empty() { return Err(Error::Tool("resource_group and instance_id required for get_metrics".into())); }
                    let start = params.get("start_time").and_then(|v| v.as_str()).map(|s| s.to_string())
                        .unwrap_or_else(|| (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339());
                    let end = params.get("end_time").and_then(|v| v.as_str()).map(|s| s.to_string())
                        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
                    let url = format!(
                        "https://management.azure.com/subscriptions/{}/resourceGroups/{}/providers/Microsoft.Compute/virtualMachines/{}/providers/microsoft.insights/metrics?api-version=2023-10-01&metricnames={}&timespan={}/{}",
                        sub_id, rg, name, urlencoding::encode(metric), urlencoding::encode(&start), urlencoding::encode(&end)
                    );
                    Self::rest_get(&client, &url, &token).await
                }
                _ => self.azure_cli_fallback(params, action, &sub_id, rg).await,
            };
        }

        self.azure_cli_fallback(params, action, &sub_id, rg).await
    }

    async fn azure_cli_fallback(&self, params: &Value, action: &str, sub_id: &str, rg: &str) -> Result<Value> {
        let sf = if sub_id.is_empty() { String::new() } else { format!(" --subscription {}", sub_id) };
        let rf = if rg.is_empty() { String::new() } else { format!(" --resource-group {}", rg) };
        let cmd = match action {
            "list_vms" => format!("az vm list{}{} --output json 2>&1", sf, rf),
            "get_vm" => {
                let name = params.get("instance_id").and_then(|v| v.as_str()).unwrap_or("");
                format!("az vm show --name {}{}{} --show-details --output json 2>&1", name, sf, rf)
            }
            "start_vm" => {
                let name = params.get("instance_id").and_then(|v| v.as_str()).unwrap_or("");
                format!("az vm start --name {}{}{} --output json 2>&1", name, sf, rf)
            }
            "stop_vm" => {
                let name = params.get("instance_id").and_then(|v| v.as_str()).unwrap_or("");
                format!("az vm deallocate --name {}{}{} --output json 2>&1", name, sf, rf)
            }
            "get_metrics" => {
                let metric = params.get("metric_name").and_then(|v| v.as_str()).unwrap_or("Percentage CPU");
                let vm = params.get("instance_id").and_then(|v| v.as_str()).unwrap_or("");
                format!("az monitor metrics list --resource {} --metric '{}'{}{} --output json 2>&1", vm, metric, sf, rf)
            }
            "get_costs" => {
                let start = params.get("start_time").and_then(|v| v.as_str()).unwrap_or("");
                let end = params.get("end_time").and_then(|v| v.as_str()).unwrap_or("");
                if start.is_empty() || end.is_empty() {
                    format!("az consumption usage list{} --top 50 --output json 2>&1", sf)
                } else {
                    format!("az consumption usage list{} --start-date {} --end-date {} --output json 2>&1", sf, start, end)
                }
            }
            "list_containers" => {
                let acct = params.get("bucket").and_then(|v| v.as_str()).unwrap_or("");
                format!("az storage container list --account-name {} --output json 2>&1", acct)
            }
            "list_blobs" => {
                let container = params.get("bucket").and_then(|v| v.as_str()).unwrap_or("");
                let prefix = params.get("prefix").and_then(|v| v.as_str()).unwrap_or("");
                if prefix.is_empty() {
                    format!("az storage blob list --container-name {} --output json 2>&1", container)
                } else {
                    format!("az storage blob list --container-name {} --prefix '{}' --output json 2>&1", container, prefix)
                }
            }
            "list_services" => format!("az container list{}{} --output json 2>&1", sf, rf),
            _ => return Err(Error::Tool(format!("Unknown Azure action: {}", action))),
        };
        Self::run_cli(&cmd).await
    }
}

fn truncate_body(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let end = s.char_indices().nth(max).map(|(i, _)| i).unwrap_or(s.len());
        format!("{}...(truncated)", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = CloudApiTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "cloud_api");
        assert!(schema.description.contains("AWS"));
        assert!(schema.description.contains("GCP"));
        assert!(schema.description.contains("Azure"));
    }

    #[test]
    fn test_validate_valid() {
        let tool = CloudApiTool;
        assert!(tool.validate(&json!({"provider": "aws", "action": "list_instances"})).is_ok());
        assert!(tool.validate(&json!({"provider": "gcp", "action": "get_metrics"})).is_ok());
        assert!(tool.validate(&json!({"provider": "azure", "action": "list_vms"})).is_ok());
    }

    #[test]
    fn test_validate_invalid_provider() {
        let tool = CloudApiTool;
        assert!(tool.validate(&json!({"provider": "alibaba", "action": "list_instances"})).is_err());
    }

    #[test]
    fn test_validate_invalid_action() {
        let tool = CloudApiTool;
        assert!(tool.validate(&json!({"provider": "aws", "action": "list_vms"})).is_err());
        assert!(tool.validate(&json!({"provider": "azure", "action": "list_instances"})).is_err());
    }

    #[test]
    fn test_validate_missing_action() {
        let tool = CloudApiTool;
        assert!(tool.validate(&json!({"provider": "aws"})).is_err());
    }

    #[test]
    fn test_truncate_body() {
        assert_eq!(truncate_body("hello", 10), "hello");
        assert!(truncate_body("hello world this is long", 5).contains("truncated"));
    }

    #[test]
    fn test_gcp_zone() {
        assert_eq!(CloudApiTool::gcp_zone("us-central1"), "us-central1-a");
        assert_eq!(CloudApiTool::gcp_zone("us-central1-b"), "us-central1-b");
    }
}