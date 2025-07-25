locals {
  temporal_worker_ai_ts_service_name = "worker-ai-ts"
  temporal_worker_ai_ts_prefix       = "${var.prefix}-${local.temporal_worker_ai_ts_service_name}"
  temporal_worker_ai_ts_param_prefix = "${local.param_prefix}/worker/ai/ts"
}

resource "aws_ssm_parameter" "temporal_worker_ai_ts_env_vars" {
  # Only put secrets into SSM
  for_each = { for env_var in var.temporal_worker_ai_ts_env_vars : env_var.name => env_var if env_var.secret }

  name = "${local.temporal_worker_ai_ts_param_prefix}/${each.value.name}"
  # Still supports non-secret values
  type      = each.value.secret ? "SecureString" : "String"
  value     = each.value.secret ? sensitive(each.value.value) : each.value.value
  overwrite = true
  tags      = {}
}

locals {
  temporal_worker_ai_ts_service_container_def = {
    essential = true
    name      = local.temporal_worker_ai_ts_prefix
    image     = "${var.temporal_worker_ai_ts_image.url}:latest"
    cpu       = 0 # let ECS divvy up the available CPU
    healthCheck = {
      command     = ["CMD", "/bin/sh", "-c", "curl -f http://localhost:4100/health || exit 1"]
      startPeriod = 10
      interval    = 10
      retries     = 10
      timeout     = 5
    }

    logConfiguration = {
      logDriver = "awslogs"
      options = {
        "awslogs-create-group"  = "true"
        "awslogs-group"         = local.log_group_name
        "awslogs-stream-prefix" = local.temporal_worker_ai_ts_service_name
        "awslogs-region"        = var.region
      }
    }
    Environment = concat(
      [
        for env_var in var.temporal_worker_ai_ts_env_vars : { name = env_var.name, value = env_var.value }
        if !env_var.secret
      ],
      [
        { name = "HASH_TEMPORAL_SERVER_HOST", value = var.temporal_host },
        { name = "HASH_TEMPORAL_SERVER_PORT", value = var.temporal_port },
        { name = "HASH_GRAPH_HTTP_HOST", value = local.graph_http_container_port_dns },
        { name = "HASH_GRAPH_HTTP_PORT", value = tostring(local.graph_http_container_port) },
        { name = "HASH_GRAPH_RPC_HOST", value = local.graph_rpc_container_port_dns },
        { name = "HASH_GRAPH_RPC_PORT", value = tostring(local.graph_rpc_container_port) },
        { name = "HASH_OTLP_ENDPOINT", value = "http://${local.otel_grpc_container_port_dns}:${local.otel_grpc_container_port}" },
      ],
    )

    secrets = [
      for env_name, ssm_param in aws_ssm_parameter.temporal_worker_ai_ts_env_vars :
      { name = env_name, valueFrom = ssm_param.arn }
    ]

    essential = true
  }
}
