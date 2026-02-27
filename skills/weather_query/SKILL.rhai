// weather_query skill — weather data via QVeris
let city = params.get("city");
if city == () { city = params.get("location"); }
if city == () { city = "北京"; }

let result = call_tool("qveris", #{
    action: "search_and_execute",
    query: city + " 当前天气 温度 湿度 风速 天气预报"
});

if result.error == () {
    result
} else {
    call_tool("web_search", #{
        query: city + " weather forecast today"
    })
}
