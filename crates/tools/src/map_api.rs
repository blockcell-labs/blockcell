use async_trait::async_trait;
use blockcell_core::{Error, Result};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::debug;

use crate::{Tool, ToolContext, ToolSchema};

/// Map and navigation tool.
///
/// Supported providers:
/// - **Google Maps**: Geocoding, reverse geocoding, directions, distance matrix, nearby search, place details
/// - **Amap (高德)**: Same capabilities via Amap Web Service API
///
/// Requires API keys via parameters, config providers section, or environment variables.
pub struct MapApiTool;

#[async_trait]
impl Tool for MapApiTool {
    fn schema(&self) -> ToolSchema {
        let mut props = serde_json::Map::new();
        props.insert("provider".into(), json!({"type": "string", "enum": ["google", "amap"], "description": "Map provider. 'google' for Google Maps Platform, 'amap' for 高德地图."}));
        props.insert("action".into(), json!({"type": "string", "description": "Action: geocode|reverse_geocode|directions|distance_matrix|nearby_search|place_details|autocomplete"}));
        props.insert("address".into(), json!({"type": "string", "description": "(geocode/autocomplete) Address string to geocode or search"}));
        props.insert("latitude".into(), json!({"type": "number", "description": "(reverse_geocode/nearby_search) Latitude coordinate"}));
        props.insert("longitude".into(), json!({"type": "number", "description": "(reverse_geocode/nearby_search) Longitude coordinate"}));
        props.insert("location".into(), json!({"type": "string", "description": "(reverse_geocode/nearby_search for amap) Location as 'longitude,latitude' string"}));
        props.insert("origin".into(), json!({"type": "string", "description": "(directions/distance_matrix) Origin address or 'lat,lng'"}));
        props.insert("destination".into(), json!({"type": "string", "description": "(directions/distance_matrix) Destination address or 'lat,lng'"}));
        props.insert("origins".into(), json!({"type": "string", "description": "(distance_matrix) Multiple origins separated by '|'"}));
        props.insert("destinations".into(), json!({"type": "string", "description": "(distance_matrix) Multiple destinations separated by '|'"}));
        props.insert("mode".into(), json!({"type": "string", "enum": ["driving", "walking", "bicycling", "transit"], "description": "(directions/distance_matrix) Travel mode. Default: driving"}));
        props.insert("keyword".into(), json!({"type": "string", "description": "(nearby_search) Search keyword (e.g. 'restaurant', 'gas station', '餐厅')"}));
        props.insert("radius".into(), json!({"type": "integer", "description": "(nearby_search) Search radius in meters. Default: 1000"}));
        props.insert("place_id".into(), json!({"type": "string", "description": "(place_details) Google place_id or Amap POI id"}));
        props.insert("language".into(), json!({"type": "string", "description": "Response language (e.g. 'en', 'zh-CN'). Default: provider default"}));
        props.insert("departure_time".into(), json!({"type": "string", "description": "(directions/distance_matrix) Departure time as ISO 8601 or 'now'"}));
        props.insert("avoid".into(), json!({"type": "string", "description": "(directions) Avoid options: tolls|highways|ferries (comma-separated)"}));
        props.insert("api_key".into(), json!({"type": "string", "description": "API key (overrides config/env)"}));
        props.insert("max_results".into(), json!({"type": "integer", "description": "(nearby_search/autocomplete) Max results. Default: 10"}));

        ToolSchema {
            name: "map_api",
            description: "Map and navigation services. Geocode addresses, get directions with travel time, calculate distance matrices, search nearby places, and get place details. Providers: 'google' (Google Maps Platform, requires GOOGLE_MAPS_API_KEY) or 'amap' (高德地图, requires AMAP_API_KEY).",
            parameters: json!({
                "type": "object",
                "properties": Value::Object(props),
                "required": ["provider", "action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let provider = params.get("provider").and_then(|v| v.as_str()).unwrap_or("");
        if !["google", "amap"].contains(&provider) {
            return Err(Error::Tool("provider must be 'google' or 'amap'".into()));
        }
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let valid = ["geocode", "reverse_geocode", "directions", "distance_matrix", "nearby_search", "place_details", "autocomplete"];
        if !valid.contains(&action) {
            return Err(Error::Tool(format!("Invalid action '{}'. Valid: {}", action, valid.join(", "))));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let provider = params["provider"].as_str().unwrap_or("google");
        let action = params["action"].as_str().unwrap_or("");
        let api_key = resolve_api_key(provider, &params, &ctx)?;

        debug!(provider = provider, action = action, "map_api execute");

        match provider {
            "google" => execute_google(action, &params, &api_key).await,
            "amap" => execute_amap(action, &params, &api_key).await,
            _ => Err(Error::Tool(format!("Unknown provider: {}", provider))),
        }
    }
}

fn resolve_api_key(provider: &str, params: &Value, ctx: &ToolContext) -> Result<String> {
    // 1. Direct param
    if let Some(key) = params.get("api_key").and_then(|v| v.as_str()) {
        if !key.is_empty() {
            return Ok(key.to_string());
        }
    }
    // 2. Config providers section
    let config_key = match provider {
        "google" => "google_maps",
        "amap" => "amap",
        _ => provider,
    };
    if let Some(pc) = ctx.config.get_provider(config_key) {
        if !pc.api_key.is_empty() {
            return Ok(pc.api_key.clone());
        }
    }
    // 3. Environment variable
    let env_key = match provider {
        "google" => "GOOGLE_MAPS_API_KEY",
        "amap" => "AMAP_API_KEY",
        _ => "",
    };
    if !env_key.is_empty() {
        if let Ok(val) = std::env::var(env_key) {
            if !val.is_empty() {
                return Ok(val);
            }
        }
    }
    Err(Error::Tool(format!(
        "No API key found for provider '{}'. Set via api_key param, config providers.{}.api_key, or {} env var.",
        provider, config_key, env_key
    )))
}

async fn execute_google(action: &str, params: &Value, api_key: &str) -> Result<Value> {
    let client = Client::new();
    let base = "https://maps.googleapis.com/maps/api";

    match action {
        "geocode" => {
            let address = params.get("address").and_then(|v| v.as_str())
                .ok_or_else(|| Error::Tool("address is required for geocode".into()))?;
            let language = params.get("language").and_then(|v| v.as_str()).unwrap_or("en");
            let url = format!("{}/geocode/json", base);
            let resp = client.get(&url)
                .query(&[("address", address), ("key", api_key), ("language", language)])
                .send().await
                .map_err(|e| Error::Tool(format!("Google Maps request failed: {}", e)))?;
            parse_google_response(resp).await
        }
        "reverse_geocode" => {
            let lat = params.get("latitude").and_then(|v| v.as_f64())
                .ok_or_else(|| Error::Tool("latitude is required for reverse_geocode".into()))?;
            let lng = params.get("longitude").and_then(|v| v.as_f64())
                .ok_or_else(|| Error::Tool("longitude is required for reverse_geocode".into()))?;
            let language = params.get("language").and_then(|v| v.as_str()).unwrap_or("en");
            let latlng = format!("{},{}", lat, lng);
            let url = format!("{}/geocode/json", base);
            let resp = client.get(&url)
                .query(&[("latlng", latlng.as_str()), ("key", api_key), ("language", language)])
                .send().await
                .map_err(|e| Error::Tool(format!("Google Maps request failed: {}", e)))?;
            parse_google_response(resp).await
        }
        "directions" => {
            let origin = params.get("origin").and_then(|v| v.as_str())
                .ok_or_else(|| Error::Tool("origin is required for directions".into()))?;
            let destination = params.get("destination").and_then(|v| v.as_str())
                .ok_or_else(|| Error::Tool("destination is required for directions".into()))?;
            let mode = params.get("mode").and_then(|v| v.as_str()).unwrap_or("driving");
            let language = params.get("language").and_then(|v| v.as_str()).unwrap_or("en");
            let url = format!("{}/directions/json", base);
            let mut query: Vec<(&str, &str)> = vec![
                ("origin", origin), ("destination", destination),
                ("mode", mode), ("key", api_key), ("language", language),
            ];
            let avoid_val;
            if let Some(avoid) = params.get("avoid").and_then(|v| v.as_str()) {
                avoid_val = avoid.to_string();
                query.push(("avoid", &avoid_val));
            }
            let dep_time;
            if let Some(dt) = params.get("departure_time").and_then(|v| v.as_str()) {
                dep_time = dt.to_string();
                query.push(("departure_time", &dep_time));
            }
            let resp = client.get(&url).query(&query).send().await
                .map_err(|e| Error::Tool(format!("Google Maps request failed: {}", e)))?;
            let body: Value = resp.json().await
                .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;
            // Extract summary
            let routes = body.get("routes").and_then(|v| v.as_array());
            if let Some(routes) = routes {
                if let Some(route) = routes.first() {
                    let legs = route.get("legs").and_then(|v| v.as_array());
                    if let Some(legs) = legs {
                        let summaries: Vec<Value> = legs.iter().map(|leg| {
                            json!({
                                "distance": leg.get("distance"),
                                "duration": leg.get("duration"),
                                "duration_in_traffic": leg.get("duration_in_traffic"),
                                "start_address": leg.get("start_address"),
                                "end_address": leg.get("end_address"),
                                "steps_count": leg.get("steps").and_then(|s| s.as_array()).map(|a| a.len()),
                            })
                        }).collect();
                        return Ok(json!({
                            "status": body.get("status"),
                            "summary": route.get("summary"),
                            "warnings": route.get("warnings"),
                            "legs": summaries,
                        }));
                    }
                }
            }
            Ok(body)
        }
        "distance_matrix" => {
            let origins = params.get("origins").and_then(|v| v.as_str())
                .or_else(|| params.get("origin").and_then(|v| v.as_str()))
                .ok_or_else(|| Error::Tool("origins is required for distance_matrix".into()))?;
            let destinations = params.get("destinations").and_then(|v| v.as_str())
                .or_else(|| params.get("destination").and_then(|v| v.as_str()))
                .ok_or_else(|| Error::Tool("destinations is required for distance_matrix".into()))?;
            let mode = params.get("mode").and_then(|v| v.as_str()).unwrap_or("driving");
            let language = params.get("language").and_then(|v| v.as_str()).unwrap_or("en");
            let url = format!("{}/distancematrix/json", base);
            let resp = client.get(&url)
                .query(&[
                    ("origins", origins), ("destinations", destinations),
                    ("mode", mode), ("key", api_key), ("language", language),
                ])
                .send().await
                .map_err(|e| Error::Tool(format!("Google Maps request failed: {}", e)))?;
            parse_google_response(resp).await
        }
        "nearby_search" => {
            let lat = params.get("latitude").and_then(|v| v.as_f64())
                .ok_or_else(|| Error::Tool("latitude is required for nearby_search".into()))?;
            let lng = params.get("longitude").and_then(|v| v.as_f64())
                .ok_or_else(|| Error::Tool("longitude is required for nearby_search".into()))?;
            let radius = params.get("radius").and_then(|v| v.as_u64()).unwrap_or(1000);
            let location = format!("{},{}", lat, lng);
            let radius_str = radius.to_string();
            let language = params.get("language").and_then(|v| v.as_str()).unwrap_or("en");
            let url = format!("{}/place/nearbysearch/json", base);
            let mut query: Vec<(&str, &str)> = vec![
                ("location", &location), ("radius", &radius_str),
                ("key", api_key), ("language", language),
            ];
            let kw;
            if let Some(keyword) = params.get("keyword").and_then(|v| v.as_str()) {
                kw = keyword.to_string();
                query.push(("keyword", &kw));
            }
            let resp = client.get(&url).query(&query).send().await
                .map_err(|e| Error::Tool(format!("Google Maps request failed: {}", e)))?;
            let body: Value = resp.json().await
                .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;
            let max_results = params.get("max_results").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            if let Some(results) = body.get("results").and_then(|v| v.as_array()) {
                let trimmed: Vec<Value> = results.iter().take(max_results).map(|r| {
                    json!({
                        "name": r.get("name"),
                        "place_id": r.get("place_id"),
                        "address": r.get("vicinity"),
                        "rating": r.get("rating"),
                        "user_ratings_total": r.get("user_ratings_total"),
                        "types": r.get("types"),
                        "location": r.get("geometry").and_then(|g| g.get("location")),
                        "open_now": r.get("opening_hours").and_then(|o| o.get("open_now")),
                    })
                }).collect();
                Ok(json!({"status": body.get("status"), "results": trimmed, "total": results.len()}))
            } else {
                Ok(body)
            }
        }
        "place_details" => {
            let place_id = params.get("place_id").and_then(|v| v.as_str())
                .ok_or_else(|| Error::Tool("place_id is required for place_details".into()))?;
            let language = params.get("language").and_then(|v| v.as_str()).unwrap_or("en");
            let url = format!("{}/place/details/json", base);
            let resp = client.get(&url)
                .query(&[
                    ("place_id", place_id), ("key", api_key), ("language", language),
                    ("fields", "name,formatted_address,formatted_phone_number,website,rating,user_ratings_total,opening_hours,geometry,types,price_level,reviews"),
                ])
                .send().await
                .map_err(|e| Error::Tool(format!("Google Maps request failed: {}", e)))?;
            parse_google_response(resp).await
        }
        "autocomplete" => {
            let input = params.get("address").and_then(|v| v.as_str())
                .ok_or_else(|| Error::Tool("address is required for autocomplete".into()))?;
            let language = params.get("language").and_then(|v| v.as_str()).unwrap_or("en");
            let url = format!("{}/place/autocomplete/json", base);
            let resp = client.get(&url)
                .query(&[("input", input), ("key", api_key), ("language", language)])
                .send().await
                .map_err(|e| Error::Tool(format!("Google Maps request failed: {}", e)))?;
            parse_google_response(resp).await
        }
        _ => Err(Error::Tool(format!("Unknown action: {}", action))),
    }
}

async fn execute_amap(action: &str, params: &Value, api_key: &str) -> Result<Value> {
    let client = Client::new();
    let base = "https://restapi.amap.com/v3";

    match action {
        "geocode" => {
            let address = params.get("address").and_then(|v| v.as_str())
                .ok_or_else(|| Error::Tool("address is required for geocode".into()))?;
            let url = format!("{}/geocode/geo", base);
            let resp = client.get(&url)
                .query(&[("address", address), ("key", api_key), ("output", "json")])
                .send().await
                .map_err(|e| Error::Tool(format!("Amap request failed: {}", e)))?;
            parse_amap_response(resp).await
        }
        "reverse_geocode" => {
            let location = if let Some(loc) = params.get("location").and_then(|v| v.as_str()) {
                loc.to_string()
            } else {
                let lng = params.get("longitude").and_then(|v| v.as_f64())
                    .ok_or_else(|| Error::Tool("longitude (or location) is required".into()))?;
                let lat = params.get("latitude").and_then(|v| v.as_f64())
                    .ok_or_else(|| Error::Tool("latitude (or location) is required".into()))?;
                format!("{},{}", lng, lat)
            };
            let url = format!("{}/geocode/regeo", base);
            let resp = client.get(&url)
                .query(&[("location", location.as_str()), ("key", api_key), ("output", "json")])
                .send().await
                .map_err(|e| Error::Tool(format!("Amap request failed: {}", e)))?;
            parse_amap_response(resp).await
        }
        "directions" => {
            let origin = params.get("origin").and_then(|v| v.as_str())
                .ok_or_else(|| Error::Tool("origin is required for directions".into()))?;
            let destination = params.get("destination").and_then(|v| v.as_str())
                .ok_or_else(|| Error::Tool("destination is required for directions".into()))?;
            let mode = params.get("mode").and_then(|v| v.as_str()).unwrap_or("driving");
            let endpoint = match mode {
                "walking" => "direction/walking",
                "bicycling" => "direction/bicycling",
                "transit" => "direction/transit/integrated",
                _ => "direction/driving",
            };
            let url = format!("{}/{}", base, endpoint);
            let resp = client.get(&url)
                .query(&[("origin", origin), ("destination", destination), ("key", api_key), ("output", "json")])
                .send().await
                .map_err(|e| Error::Tool(format!("Amap request failed: {}", e)))?;
            let body: Value = resp.json().await
                .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;
            // Extract route summary
            if let Some(route) = body.get("route") {
                let paths = route.get("paths").and_then(|v| v.as_array())
                    .or_else(|| route.get("transits").and_then(|v| v.as_array()));
                if let Some(paths) = paths {
                    let summaries: Vec<Value> = paths.iter().map(|p| {
                        json!({
                            "distance": p.get("distance"),
                            "duration": p.get("duration"),
                            "strategy": p.get("strategy"),
                            "toll_distance": p.get("toll_distance"),
                            "tolls": p.get("tolls"),
                            "traffic_lights": p.get("traffic_lights"),
                            "steps_count": p.get("steps").and_then(|s| s.as_array()).map(|a| a.len()),
                        })
                    }).collect();
                    return Ok(json!({
                        "status": body.get("status"),
                        "info": body.get("info"),
                        "origin": route.get("origin"),
                        "destination": route.get("destination"),
                        "paths": summaries,
                    }));
                }
            }
            Ok(body)
        }
        "distance_matrix" => {
            let origins = params.get("origins").and_then(|v| v.as_str())
                .or_else(|| params.get("origin").and_then(|v| v.as_str()))
                .ok_or_else(|| Error::Tool("origins is required for distance_matrix".into()))?;
            let destinations = params.get("destinations").and_then(|v| v.as_str())
                .or_else(|| params.get("destination").and_then(|v| v.as_str()))
                .ok_or_else(|| Error::Tool("destinations is required for distance_matrix".into()))?;
            let url = format!("{}/distance", base);
            let mode_str = match params.get("mode").and_then(|v| v.as_str()).unwrap_or("driving") {
                "driving" => "1",
                "walking" => "3",
                _ => "1",
            };
            let resp = client.get(&url)
                .query(&[
                    ("origins", origins), ("destination", destinations),
                    ("type", mode_str), ("key", api_key), ("output", "json"),
                ])
                .send().await
                .map_err(|e| Error::Tool(format!("Amap request failed: {}", e)))?;
            parse_amap_response(resp).await
        }
        "nearby_search" => {
            let location = if let Some(loc) = params.get("location").and_then(|v| v.as_str()) {
                loc.to_string()
            } else {
                let lng = params.get("longitude").and_then(|v| v.as_f64())
                    .ok_or_else(|| Error::Tool("longitude (or location) is required".into()))?;
                let lat = params.get("latitude").and_then(|v| v.as_f64())
                    .ok_or_else(|| Error::Tool("latitude (or location) is required".into()))?;
                format!("{},{}", lng, lat)
            };
            let radius = params.get("radius").and_then(|v| v.as_u64()).unwrap_or(1000);
            let radius_str = radius.to_string();
            let max_results = params.get("max_results").and_then(|v| v.as_u64()).unwrap_or(10);
            let offset_str = max_results.to_string();
            let url = format!("{}/place/around", base);
            let mut query: Vec<(&str, &str)> = vec![
                ("location", &location), ("radius", &radius_str),
                ("key", api_key), ("output", "json"), ("offset", &offset_str),
            ];
            let kw;
            if let Some(keyword) = params.get("keyword").and_then(|v| v.as_str()) {
                kw = keyword.to_string();
                query.push(("keywords", &kw));
            }
            let resp = client.get(&url).query(&query).send().await
                .map_err(|e| Error::Tool(format!("Amap request failed: {}", e)))?;
            let body: Value = resp.json().await
                .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;
            if let Some(pois) = body.get("pois").and_then(|v| v.as_array()) {
                let trimmed: Vec<Value> = pois.iter().map(|p| {
                    json!({
                        "name": p.get("name"),
                        "id": p.get("id"),
                        "address": p.get("address"),
                        "type": p.get("type"),
                        "location": p.get("location"),
                        "tel": p.get("tel"),
                        "distance": p.get("distance"),
                        "rating": p.get("biz_ext").and_then(|b| b.get("rating")),
                    })
                }).collect();
                Ok(json!({"status": body.get("status"), "results": trimmed, "count": body.get("count")}))
            } else {
                Ok(body)
            }
        }
        "place_details" => {
            let place_id = params.get("place_id").and_then(|v| v.as_str())
                .ok_or_else(|| Error::Tool("place_id is required for place_details".into()))?;
            let url = format!("{}/place/detail", base);
            let resp = client.get(&url)
                .query(&[("id", place_id), ("key", api_key), ("output", "json")])
                .send().await
                .map_err(|e| Error::Tool(format!("Amap request failed: {}", e)))?;
            parse_amap_response(resp).await
        }
        "autocomplete" => {
            let input = params.get("address").and_then(|v| v.as_str())
                .ok_or_else(|| Error::Tool("address is required for autocomplete".into()))?;
            let url = format!("{}/assistant/inputtips", base);
            let resp = client.get(&url)
                .query(&[("keywords", input), ("key", api_key), ("output", "json")])
                .send().await
                .map_err(|e| Error::Tool(format!("Amap request failed: {}", e)))?;
            parse_amap_response(resp).await
        }
        _ => Err(Error::Tool(format!("Unknown action: {}", action))),
    }
}

async fn parse_google_response(resp: reqwest::Response) -> Result<Value> {
    let status = resp.status();
    let body: Value = resp.json().await
        .map_err(|e| Error::Tool(format!("Failed to parse Google Maps response: {}", e)))?;
    if !status.is_success() {
        return Err(Error::Tool(format!("Google Maps API error ({}): {:?}", status, body.get("error_message"))));
    }
    Ok(body)
}

async fn parse_amap_response(resp: reqwest::Response) -> Result<Value> {
    let status = resp.status();
    let body: Value = resp.json().await
        .map_err(|e| Error::Tool(format!("Failed to parse Amap response: {}", e)))?;
    if !status.is_success() {
        return Err(Error::Tool(format!("Amap API error ({}): {:?}", status, body.get("info"))));
    }
    let amap_status = body.get("status").and_then(|v| v.as_str()).unwrap_or("0");
    if amap_status != "1" {
        return Err(Error::Tool(format!("Amap API error: {}", body.get("info").and_then(|v| v.as_str()).unwrap_or("unknown"))));
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_tool() -> MapApiTool { MapApiTool }

    #[test]
    fn test_schema() {
        let tool = make_tool();
        let schema = tool.schema();
        assert_eq!(schema.name, "map_api");
        let params = &schema.parameters;
        assert!(params["properties"]["provider"].is_object());
        assert!(params["properties"]["action"].is_object());
    }

    #[test]
    fn test_validate_valid() {
        let tool = make_tool();
        assert!(tool.validate(&json!({"provider": "google", "action": "geocode"})).is_ok());
        assert!(tool.validate(&json!({"provider": "amap", "action": "directions"})).is_ok());
        assert!(tool.validate(&json!({"provider": "amap", "action": "nearby_search"})).is_ok());
    }

    #[test]
    fn test_validate_invalid_provider() {
        let tool = make_tool();
        assert!(tool.validate(&json!({"provider": "bing", "action": "geocode"})).is_err());
    }

    #[test]
    fn test_validate_invalid_action() {
        let tool = make_tool();
        assert!(tool.validate(&json!({"provider": "google", "action": "fly"})).is_err());
    }

    #[test]
    fn test_validate_all_actions() {
        let tool = make_tool();
        for action in &["geocode", "reverse_geocode", "directions", "distance_matrix", "nearby_search", "place_details", "autocomplete"] {
            assert!(tool.validate(&json!({"provider": "google", "action": action})).is_ok());
            assert!(tool.validate(&json!({"provider": "amap", "action": action})).is_ok());
        }
    }
}
