terraform {
  required_version = ">= 1.6"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.0"
    }
  }

  # Uncomment and fill in before running `terraform init` in a team/CI environment.
  # backend "s3" {
  #   bucket         = "<your-tfstate-bucket>"
  #   key            = "nitro-enc-svc/<environment>/terraform.tfstate"
  #   region         = "<your-region>"
  #   dynamodb_table = "<your-lock-table>"
  #   encrypt        = true
  # }
}

provider "aws" {
  region = var.aws_region

  default_tags {
    tags = {
      Project     = "nitro-enc-svc"
      Environment = var.environment
      ManagedBy   = "terraform"
    }
  }
}
