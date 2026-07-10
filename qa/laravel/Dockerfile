# Real Laravel app with resonance-laravel installed from the local path repo.
# Build context is the REPO ROOT (so it can copy resonance-laravel/).
# Parametrized to test the compat matrix: PHP_IMAGE × LARAVEL constraint.
ARG PHP_IMAGE=php:8.3-cli
FROM ${PHP_IMAGE}
ARG LARAVEL=laravel/laravel

RUN apt-get update && apt-get install -y git unzip && rm -rf /var/lib/apt/lists/*
ADD https://github.com/mlocati/docker-php-extension-installer/releases/latest/download/install-php-extensions /usr/local/bin/
RUN chmod +x /usr/local/bin/install-php-extensions \
    && install-php-extensions mbstring bcmath zip @composer

WORKDIR /build
RUN composer create-project ${LARAVEL} app --no-interaction --prefer-dist

WORKDIR /build/app
COPY resonance-laravel /package
RUN composer config repositories.resonance path /package \
    && composer require resonance/resonance-laravel:"*" --no-interaction

COPY qa/laravel/TestEvent.php app/Events/TestEvent.php

# Fire a real broadcast through the resonance driver.
CMD ["php", "artisan", "tinker", "--execute", "broadcast(new App\\Events\\TestEvent('hello-from-laravel'));"]
