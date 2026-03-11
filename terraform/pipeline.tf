# ── CodeBuild log groups ──────────────────────────────────────────────────────

resource "aws_cloudwatch_log_group" "codebuild" {
  name              = "/aws/codebuild/nitro-enc-svc-${var.environment}"
  retention_in_days = 90

  tags = {
    Name = "nitro-enc-svc-${var.environment}-codebuild"
  }
}

resource "aws_cloudwatch_log_group" "codebuild_test" {
  name              = "/aws/codebuild/nitro-enc-svc-${var.environment}-test"
  retention_in_days = 90

  tags = {
    Name = "nitro-enc-svc-${var.environment}-codebuild-test"
  }
}

# ── CodeBuild project ─────────────────────────────────────────────────────────

resource "aws_codebuild_project" "build" {
  name          = "nitro-enc-svc-${var.environment}"
  description   = "Builds enclave runner + vsock-proxy images and produces EIF PCR values"
  service_role  = aws_iam_role.codebuild.arn
  build_timeout = 60 # minutes

  source {
    type      = "CODEPIPELINE"
    buildspec = "buildspec.yml"
  }

  environment {
    type                        = "LINUX_CONTAINER"
    compute_type                = "BUILD_GENERAL1_MEDIUM"
    image                       = "aws/codebuild/amazonlinux2-x86_64-standard:5.0"
    image_pull_credentials_type = "CODEBUILD"
    privileged_mode             = true # required for Docker daemon

    environment_variable {
      name  = "ECR_REGISTRY"
      value = "${var.account_id}.dkr.ecr.${var.aws_region}.amazonaws.com"
    }

    environment_variable {
      name  = "ECR_REPO_RUNNER"
      value = aws_ecr_repository.runner.name
    }

    environment_variable {
      name  = "ECR_REPO_PROXY"
      value = aws_ecr_repository.vsock_proxy.name
    }

    environment_variable {
      name  = "SECRET_ARN"
      value = aws_secretsmanager_secret.dek.arn
    }

    environment_variable {
      name  = "KMS_KEY_ID"
      value = aws_kms_key.enclave_dek.arn
    }

    environment_variable {
      name  = "S3_BUCKET"
      value = aws_s3_bucket.schemas.id
    }

    environment_variable {
      name  = "VSOCK_PROXY_CID"
      value = tostring(var.vsock_proxy_cid)
    }

    environment_variable {
      name  = "OTEL_EXPORTER_OTLP_ENDPOINT"
      value = var.otel_otlp_endpoint
    }

    # Service configuration overrides (all have sane defaults in the service)
    environment_variable {
      name  = "LOG_LEVEL"
      value = var.log_level
    }

    environment_variable {
      name  = "S3_PREFIX"
      value = var.s3_prefix
    }

    environment_variable {
      name  = "SCHEMA_HEADER_NAME"
      value = var.schema_header_name
    }

    environment_variable {
      name  = "DEK_ROTATION_INTERVAL_SECS"
      value = tostring(var.dek_rotation_interval_secs)
    }

    environment_variable {
      name  = "SCHEMA_REFRESH_INTERVAL_SECS"
      value = tostring(var.schema_refresh_interval_secs)
    }

    environment_variable {
      name  = "VSOCK_PROXY_PORT"
      value = tostring(var.vsock_proxy_port)
    }

    environment_variable {
      name  = "TLS_PORT"
      value = tostring(var.tls_port)
    }

    environment_variable {
      name  = "ENCLAVE_CID"
      value = tostring(var.enclave_cid)
    }
  }

  artifacts {
    type = "CODEPIPELINE"
  }

  logs_config {
    cloudwatch_logs {
      group_name  = aws_cloudwatch_log_group.codebuild.name
      stream_name = "build"
      status      = "ENABLED"
    }

    s3_logs {
      status              = "ENABLED"
      location            = "${aws_s3_bucket.pipeline_artifacts.id}/codebuild-logs"
      encryption_disabled = false
    }
  }

  tags = {
    Name = "nitro-enc-svc-${var.environment}-build"
  }
}

# ── CodeBuild test project (VPC-enabled; reaches internal NLB for smoke tests) ─

resource "aws_security_group" "codebuild_test" {
  name        = "${var.cluster_name}-codebuild-test"
  description = "Egress-only SG for the post-deploy test CodeBuild project"
  vpc_id      = aws_vpc.main.id

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }

  tags = {
    Name = "${var.cluster_name}-codebuild-test"
  }
}

resource "aws_codebuild_project" "test" {
  name          = "nitro-enc-svc-${var.environment}-test"
  description   = "Post-deploy smoke tests: health, encrypt, ab load test against the internal NLB"
  service_role  = aws_iam_role.codebuild_test.arn
  build_timeout = 30

  vpc_config {
    vpc_id             = aws_vpc.main.id
    subnets            = aws_subnet.private[*].id
    security_group_ids = [aws_security_group.codebuild_test.id]
  }

  source {
    type      = "CODEPIPELINE"
    buildspec = "buildspec-test.yml"
  }

  artifacts {
    type = "CODEPIPELINE"
  }

  environment {
    type         = "LINUX_CONTAINER"
    compute_type = "BUILD_GENERAL1_SMALL"
    image        = "aws/codebuild/standard:7.0"

    environment_variable {
      name  = "CLUSTER_NAME"
      value = var.cluster_name
    }

    environment_variable {
      name  = "AWS_REGION"
      value = var.aws_region
    }
  }

  logs_config {
    cloudwatch_logs {
      group_name  = aws_cloudwatch_log_group.codebuild_test.name
      stream_name = "test"
      status      = "ENABLED"
    }
  }

  tags = {
    Name = "nitro-enc-svc-${var.environment}-test"
  }
}

# ── CodePipeline ──────────────────────────────────────────────────────────────

resource "aws_codepipeline" "pipeline" {
  name     = "nitro-enc-svc-${var.environment}"
  role_arn = aws_iam_role.codepipeline.arn

  artifact_store {
    location = aws_s3_bucket.pipeline_artifacts.id
    type     = "S3"

    encryption_key {
      id   = aws_kms_key.enclave_dek.arn
      type = "KMS"
    }
  }

  # ── Stage 1: Source (GitHub via CodeStar connection) ──────────────────────

  stage {
    name = "Source"

    action {
      name             = "GitHub"
      category         = "Source"
      owner            = "AWS"
      provider         = "CodeStarSourceConnection"
      version          = "1"
      output_artifacts = ["source_output"]

      configuration = {
        ConnectionArn        = var.codestar_connection_arn
        FullRepositoryId     = var.source_repo_id
        BranchName           = var.source_repo_branch
        OutputArtifactFormat = "CODE_ZIP"
        DetectChanges        = "true"
      }
    }
  }

  # ── Stage 2: Build (CodeBuild — compiles, builds EIF, extracts PCR values) ─

  stage {
    name = "Build"

    action {
      name             = "BuildAndPackage"
      category         = "Build"
      owner            = "AWS"
      provider         = "CodeBuild"
      version          = "1"
      input_artifacts  = ["source_output"]
      output_artifacts = ["build_output"]

      configuration = {
        ProjectName = aws_codebuild_project.build.name
      }
    }
  }

  # ── Stage 3: Manual approval (review PCR0 before deploying to production) ──

  stage {
    name = "Approve"

    action {
      name     = "ReviewPCR0"
      category = "Approval"
      owner    = "AWS"
      provider = "Manual"
      version  = "1"

      configuration = {
        CustomData = "Review enclave/build-summary.json — verify PCR0 matches expected value and update kms_enclave_pcr0 in Terraform before approving."
      }
    }
  }

  # ── Stage 4: Post-deploy smoke tests ──────────────────────────────────────

  stage {
    name = "DeployAndTest"

    action {
      name            = "RunTests"
      category        = "Build"
      owner           = "AWS"
      provider        = "CodeBuild"
      version         = "1"
      input_artifacts = ["source_output"]

      configuration = {
        ProjectName = aws_codebuild_project.test.name
      }
    }
  }

  tags = {
    Name = "nitro-enc-svc-${var.environment}"
  }

  depends_on = [
    aws_iam_role_policy.codepipeline_policy,
  ]
}
