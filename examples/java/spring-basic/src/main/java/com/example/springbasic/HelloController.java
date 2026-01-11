package com.example.springbasic;

import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.PathVariable;
import org.springframework.web.bind.annotation.RestController;
import java.util.HashMap;
import java.util.Map;

@RestController
public class HelloController {

    @GetMapping("/")
    public Map<String, String> hello() {
        Map<String, String> response = new HashMap<>();
        response.put("message", "Hello from Spring Boot!");
        response.put("status", "healthy");
        response.put("version", "1.0.0");
        return response;
    }
    @GetMapping("/{name}")
    public Map<String, String> hello(@PathVariable String name) {
        Map<String, String> response = new HashMap<>();
        response.put("message", "Hello from Spring Boot! " + name);
        response.put("status", "healthy");
        response.put("version", "1.0.0");
        return response;
    }

    @GetMapping("/health")
    public Map<String, String> health() {
        Map<String, String> response = new HashMap<>();
        response.put("status", "ok");
        return response;
    }
}
